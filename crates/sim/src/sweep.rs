//! パラメータの列挙・抽出・並列実行（spec 11 章）。
//!
//! 組み合わせの添字は混合基数（mixed radix）で表す: パラメータは `ParamSpec` のスライスの
//! 順番（`CompiledScript::params` は名前の辞書順）をそのまま桁の重みとして使い、**スライスの
//! 最後の要素を最下位の桁**とする（spec 11 章: 「辞書順で最後のパラメータを最下位の桁とする」）。
//! 全組み合わせ数は `checked_mul` で桁あふれを検査しながら求める。積が `2^63`
//! （`MAX_TOTAL_COMBINATIONS`）を超えるか、`u128` 自体がオーバーフローした場合は、
//! `total_combinations`・`default_index`・`select_indices` のいずれも `SimError::Args` を
//! 返して探索を拒否する（spec 11 章: パラメータの範囲を狭めるか刻みを粗くするよう求める）。
//!
//! 抽出（`select_indices`）は spec 11 章の規則どおり、`default` の組み合わせを必ず含め、
//! 残りは `default` の添字を除いた添字の列から `rand::seq::index::sample` で抽出する。
//! シードを固定した `ChaCha8Rng` を使うため、同じ `seed` なら常に同じ結果になる。
//!
//! 並列実行（`run_sweep`）は `jobs` 本のスレッドで `indices` を分担し、完了したものから
//! 呼び出し元の `on_result` を直列に呼ぶ。`on_result` が `Err` を返した時点で、以後の
//! `on_result` の呼び出しをやめ、まだ着手していない実行を打ち切る（既に実行中のものは
//! 完了まで走る: 1 回の `run_one` を途中で中断する手段がないため、これは意図的な妥協である）。
//! ワーカースレッドが panic した場合は `catch_unwind` で捕捉し、`run_sweep` 自身を panic させず
//! `SimError::BatchFailed` を返す。

use crate::benchmark::Benchmark;
use crate::config::SimConfig;
use crate::engine::{RunStatus, simulate};
use crate::error::SimError;
use crate::eval::{self, Metrics};
use crate::script::{
    CompiledScript, ParamKind, ParamSet, ParamSpec, ParamValue, ScriptHost, ScriptRun,
};
use crate::series::Dataset;
use rand::SeedableRng;
use rand::seq::index as rand_index;
use rand_chacha::ChaCha8Rng;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

/// `run_sweep` のワーカー間で共有する作業キューの状態。`next` と `cancelled` は
/// 必ず同じロックの中で読み書きする(キャンセル後の新規着手を防ぐため)。
struct WorkQueue {
    /// 次に処理する `indices` の添字。
    next: usize,
    /// `on_result` の Err またはワーカーの panic により、新しい作業を取らせない状態。
    cancelled: bool,
}

/// 全組み合わせ数の上限（spec 11 章: `2^63`）。
const MAX_TOTAL_COMBINATIONS: u128 = 1u128 << 63;

/// 小数の格子判定の許容誤差。`script.rs::FLOAT_TOLERANCE` と同じ値（spec 8.1/11 章: 小数は
/// `1e-9` の差を許容する）だが、`script.rs` のその定数は private であり、既存ファイルを
/// 変更できない制約（計画 Global Constraints）があるため、ここで同じ値を複製して使う。
const FLOAT_GRID_TOLERANCE: f64 = 1e-9;

/// `candidate_count` の小数分岐で、商から求めた近似の k を上下に補正する回数の上限。
const MAX_FLOAT_K_ADJUSTMENTS: usize = 4;

// ---------------------------------------------------------------------------
// 列挙
// ---------------------------------------------------------------------------

/// `spec` の候補値の個数（spec 11 章: `min + k*step` のうち `max` 以下のもの）。
pub fn candidate_count(spec: &ParamSpec) -> u64 {
    match spec.kind {
        ParamKind::Int => {
            let min = spec.min as i64;
            let max = spec.max as i64;
            let step = spec.step as i64;
            // i64 同士の差は min=i64::MIN, max=i64::MAX でオーバーフローし得るため i128 で計算する
            // (script.rs の刻み判定と同じ理由)。step > 0 は 8.1 の登録時検証で保証済み。
            let span = max as i128 - min as i128;
            ((span / step as i128) as u64).saturating_add(1)
        }
        ParamKind::Float => {
            // spec 11 章: 「`min + k*step <= max + 1e-9` を満たす最大の k」。許容誤差は値の空間で
            // 判定する必要があるため、商から求めた近似の k を、`param_value_at` と同じ式
            // `min + k*step` の値で上下に調整して定義と厳密に一致させる。
            let fits = |k: u64| spec.min + k as f64 * spec.step <= spec.max + FLOAT_GRID_TOLERANCE;
            let mut k = ((spec.max - spec.min) / spec.step).floor() as u64;
            // 近似の誤差は高々数個分。k が 2^53 を超える極端な格子で f64 の丸めにより
            // 調整が終わらなくなることを避けるため、調整回数に上限を置く。
            for _ in 0..MAX_FLOAT_K_ADJUSTMENTS {
                if k > 0 && !fits(k) {
                    k -= 1;
                } else {
                    break;
                }
            }
            for _ in 0..MAX_FLOAT_K_ADJUSTMENTS {
                match k.checked_add(1) {
                    Some(next) if fits(next) => k = next,
                    _ => break,
                }
            }
            k.saturating_add(1)
        }
    }
}

/// 全組み合わせ数が上限を超えた場合のエラー（spec 11 章: パラメータの範囲を狭めるか刻みを
/// 粗くするよう求める）。`checked_mul` のオーバーフロー（理論上の安全網。本実装では各乗算の
/// 直後に `MAX_TOTAL_COMBINATIONS` 以下であることを確認しているため、次の乗算の入力は常に
/// `MAX_TOTAL_COMBINATIONS` 以下に収まり、実際にはこの経路で `u128` がオーバーフローすることは
/// ない）と、積が `MAX_TOTAL_COMBINATIONS` を超えた場合の両方で使う共通のエラーを返す。
fn too_many_combinations_error() -> SimError {
    SimError::Args(format!(
        "sweep has more than {MAX_TOTAL_COMBINATIONS} parameter combinations; narrow the \
         parameter ranges or use a coarser step to reduce the candidate count"
    ))
}

/// `specs` の全組み合わせ数（spec 11 章）。各パラメータの `candidate_count` の積を
/// `checked_mul` で桁あふれを検査しながら求め、積が `2^63`（`MAX_TOTAL_COMBINATIONS`）を
/// 超えるか `u128` がオーバーフローした場合は `SimError::Args` を返す。
/// パラメータが 0 個なら `Ok(1)`（空の組み合わせが 1 つだけ存在する）。
pub fn total_combinations(specs: &[ParamSpec]) -> Result<u128, SimError> {
    let mut total: u128 = 1;
    for spec in specs {
        let count = candidate_count(spec) as u128;
        total = total
            .checked_mul(count)
            .filter(|&t| t <= MAX_TOTAL_COMBINATIONS)
            .ok_or_else(too_many_combinations_error)?;
    }
    Ok(total)
}

/// `spec` の `default` が指す候補の位置（0 始まり）。`params()` の登録時検証（spec 8.1）で
/// `default = min + k*step`（整数は厳密に、小数は `1e-9` の許容誤差で）が保証済みなので、
/// ここでは逆算するだけでよい。
fn default_k(spec: &ParamSpec) -> u64 {
    match spec.kind {
        ParamKind::Int => {
            let min = spec.min as i64;
            let step = spec.step as i64;
            let default = spec.default as i64;
            ((default as i128 - min as i128) / step as i128) as u64
        }
        ParamKind::Float => (((spec.default - spec.min) / spec.step).round()) as u64,
    }
}

/// `k` 番目（0 始まり）の候補値を `spec.kind` に応じた `ParamValue` にする。
fn param_value_at(spec: &ParamSpec, k: u64) -> ParamValue {
    match spec.kind {
        ParamKind::Int => {
            let min = spec.min as i64;
            let step = spec.step as i64;
            // k は呼び出し元(combination_at)が candidate_count の範囲内に収めている。
            let value = min as i128 + k as i128 * step as i128;
            ParamValue::Int(value as i64)
        }
        ParamKind::Float => ParamValue::Float(spec.min + k as f64 * spec.step),
    }
}

/// `specs` の `default` の組み合わせの添字（spec 11 章の混合基数で、スライスの最後の要素が
/// 最下位の桁）。`total_combinations` と同じ桁あふれ検査を行ってから計算するため、全組み合わせ
/// 数が `2^63` を超えるスクリプトに対しては `SimError::Args` を返す。
pub fn default_index(specs: &[ParamSpec]) -> Result<u128, SimError> {
    total_combinations(specs)?;
    let mut result: u128 = 0;
    for spec in specs {
        let radix = candidate_count(spec) as u128;
        result = result * radix + default_k(spec) as u128;
    }
    Ok(result)
}

/// 添字 `index` が指す組み合わせ（spec 11 章の混合基数の逆変換）。`specs` が空なら空の
/// `ParamSet` を返す。
pub fn combination_at(specs: &[ParamSpec], index: u128) -> ParamSet {
    let mut remaining = index;
    let mut ks = vec![0u64; specs.len()];
    // スライスの最後の要素が最下位の桁(spec 11 章)なので、末尾から基数変換する。
    for i in (0..specs.len()).rev() {
        let radix = candidate_count(&specs[i]) as u128;
        ks[i] = (remaining % radix) as u64;
        remaining /= radix;
    }
    specs
        .iter()
        .zip(ks)
        .map(|(spec, k)| (spec.name.clone(), param_value_at(spec, k)))
        .collect()
}

// ---------------------------------------------------------------------------
// 抽出
// ---------------------------------------------------------------------------

/// 実行する組み合わせの添字を選ぶ（spec 11 章）。先頭は常に `default_index(specs)`。
/// `total_combinations` と同じ桁あふれ検査を行ってから抽出するため、全組み合わせ数が `2^63`
/// を超えるスクリプトに対しては `SimError::Args` を返す。
///
/// - 全組み合わせ数が `max_runs` 以下なら全件を返す（重複なし、先頭が `default`）。
/// - 超える場合は `default` を除いた添字の列から
///   `rand::seq::index::sample(&mut ChaCha8Rng::seed_from_u64(seed), 全組み合わせ数 - 1,
///   max_runs - 1)` で抽出する。同じ `seed` なら常に同じ結果になる。
///
/// `max_runs == 0` は CLI 側（spec 13 章: `--max-runs` は 1 以上）が検証する範囲外の入力だが、
/// ここで呼ばれても panic せず空の `Vec` を返す。
pub fn select_indices(
    specs: &[ParamSpec],
    max_runs: usize,
    seed: u64,
) -> Result<Vec<u128>, SimError> {
    let total = total_combinations(specs)?;
    let default_idx = default_index(specs)?;

    if max_runs == 0 {
        return Ok(Vec::new());
    }

    if total <= max_runs as u128 {
        let mut indices = Vec::with_capacity(total as usize);
        indices.push(default_idx);
        for i in 0..total {
            if i != default_idx {
                indices.push(i);
            }
        }
        return Ok(indices);
    }

    let pool_len = total - 1; // default を除いた添字の個数
    let take = max_runs - 1;
    let pool_len_usize: usize = pool_len
        .try_into()
        .expect("total_combinations is capped at 2^63, which fits in usize on our 64-bit targets");

    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let sampled = rand_index::sample(&mut rng, pool_len_usize, take);

    let mut indices = Vec::with_capacity(max_runs);
    indices.push(default_idx);
    for j in sampled.into_iter() {
        let j = j as u128;
        // サンプリングは「default を除いた」添字の列(0..pool_len)に対して行っているため、
        // default_idx 以上の値は元の添字空間に戻すために 1 つ後ろへずらす。
        let actual = if j < default_idx { j } else { j + 1 };
        indices.push(actual);
    }
    Ok(indices)
}

// ---------------------------------------------------------------------------
// 並列実行
// ---------------------------------------------------------------------------

/// panic payload から人間が読めるメッセージを取り出す（`&str` / `String` 以外は固定文言）。
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// 1 回のシミュレーションの結果一式（spec 12 章の `sim_runs` への保存に使う最小限の情報）。
/// `status` が `RunStatus::Ok` のときだけ `metrics` が `Some` になる（`run_one` の不変条件）。
#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    pub params: ParamSet,
    pub status: RunStatus,
    pub metrics: Option<Metrics>,
}

/// `params` で 1 回シミュレーションし、`status` が `Ok` の場合だけ評価指標を計算する
/// （spec 12 章: `script_error` の行は `metrics` 列自体が NULL になるため、計算しても使われない）。
pub fn run_one(
    dataset: &Arc<Dataset>,
    host: &ScriptHost,
    script: &CompiledScript,
    params: &ParamSet,
    benchmarks: &[Benchmark],
    cfg: &SimConfig,
) -> RunRecord {
    let mut run = ScriptRun::new(host, script, params, dataset.clone(), cfg);
    let outcome = simulate(dataset, &mut run, cfg);
    let metrics = match &outcome.status {
        RunStatus::Ok => Some(eval::evaluate(dataset.eval_bars(), &outcome, benchmarks)),
        RunStatus::ScriptError { .. } => None,
    };
    RunRecord {
        params: params.clone(),
        status: outcome.status,
        metrics,
    }
}

/// `indices` の各組み合わせを `jobs` 本のスレッドで並列に実行し、完了したものから
/// `on_result` を直列に呼ぶ（spec 11 章: 実行は `jobs` 本のスレッドで並列に行う）。
///
/// - `on_result` が `Err` を返したら、以後 `on_result` を呼ばず、まだ着手していない
///   （作業キューから取り出していない）実行を打ち切り、その `Err` をそのまま返す。
///   作業の取得とキャンセルの設定は同じ `Mutex` の中で行うため、キャンセル設定の後に
///   新しい実行が開始されることはない（既に着手済みの実行は完了まで走る）。
/// - ワーカースレッドが panic した場合は、その panic をスレッド内で `catch_unwind` して
///   `run_sweep` 自身を panic させず、`SimError::BatchFailed` を返す。
///
/// `indices` の順序そのものは結果の内容に影響しない（`simulate`/`evaluate` は入力が同じなら
/// 常に同じ結果になる。spec 9.2 章）。`jobs` が 0 の場合は 1 として扱う。
///
/// 引数 8 個は計画 Task 8 Interfaces 節で定められた公開 API そのものであり、分割すると
/// 呼び出し元(Task 9 の CLI)との契約が変わるため減らさない。
#[allow(clippy::too_many_arguments)]
pub fn run_sweep(
    dataset: &Arc<Dataset>,
    host: &ScriptHost,
    script: &CompiledScript,
    benchmarks: &[Benchmark],
    cfg: &SimConfig,
    indices: &[u128],
    jobs: usize,
    on_result: &mut (dyn FnMut(RunRecord) -> Result<(), SimError> + Send),
) -> Result<(), SimError> {
    if indices.is_empty() {
        return Ok(());
    }
    let jobs = jobs.max(1);

    // queue: 次に処理する indices の添字と、キャンセル済みかどうかを 1 つの Mutex で保護する。
    // 「cancelled を確認する」と「next を取って進める」を同じロックの中で行うことで、
    // main が cancelled を立てた後に、ワーカーが未着手の実行を取得して開始する競合を防ぐ。
    // panicked: いずれかのワーカーが panic したことを main 側に伝える合図。
    let queue = Mutex::new(WorkQueue {
        next: 0,
        cancelled: false,
    });
    let panicked = AtomicBool::new(false);
    let first_panic: Mutex<Option<(u128, String)>> = Mutex::new(None);
    let (tx, rx) = mpsc::channel::<RunRecord>();

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let tx = tx.clone();
            let queue = &queue;
            let panicked = &panicked;
            let first_panic = &first_panic;
            scope.spawn(move || {
                loop {
                    // ロックは作業の取得の間だけ保持し、run_one の実行中は保持しない。
                    let i = {
                        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                        if q.cancelled || q.next >= indices.len() {
                            break;
                        }
                        let i = q.next;
                        q.next += 1;
                        i
                    };
                    let idx = indices[i];
                    // run_one 自体は panic しない設計だが、ワーカー内の panic が
                    // thread::scope を通じて呼び出し元を panic させることを防ぐための
                    // 安全網として catch_unwind する(spec 12 章: スレッドの panic を含む
                    // 中断は BatchFailed として報告する)。
                    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        let params = combination_at(&script.params, idx);
                        run_one(dataset, host, script, &params, benchmarks, cfg)
                    }));
                    match result {
                        Ok(record) => {
                            if tx.send(record).is_err() {
                                // receiver が既に処理を終えてドロップしている(通常は
                                // 起こらないが、安全のため送信失敗時はこのワーカーも抜ける)。
                                break;
                            }
                        }
                        Err(payload) => {
                            let message = panic_message(payload.as_ref());
                            // 最初に panic した添字とメッセージだけを残す(複数ワーカーが
                            // 同時に panic しても、原因の特定に必要なのは最初の 1 件)。
                            let mut slot = first_panic.lock().unwrap_or_else(|e| e.into_inner());
                            if slot.is_none() {
                                *slot = Some((idx, message));
                            }
                            drop(slot);
                            panicked.store(true, Ordering::SeqCst);
                            queue.lock().unwrap_or_else(|e| e.into_inner()).cancelled = true;
                            break;
                        }
                    }
                }
            });
        }
        // 各ワーカーに clone 済みの送信端を渡したので、ここで持つ分は不要。これを drop しないと
        // 全ワーカー終了後も rx.recv() がチャンネルの終了を検出できない。
        drop(tx);

        let mut received = 0usize;
        let mut final_err: Option<SimError> = None;
        while received < indices.len() {
            match rx.recv() {
                Ok(record) => {
                    received += 1;
                    // final_err が Some の間は、既に走っていたワーカーが送ってくる残りの
                    // 結果を on_result に渡さずに読み捨てる(「以後 on_result を呼ばない」
                    // という契約を、既着手の実行の完了を待たずに満たすため)。
                    if final_err.is_some() {
                        continue;
                    }
                    if let Err(e) = on_result(record) {
                        final_err = Some(e);
                        queue.lock().unwrap_or_else(|e| e.into_inner()).cancelled = true;
                    }
                }
                Err(_) => {
                    // 全ワーカーが終了してチャンネルが閉じた(cancel か panic による早期終了)。
                    break;
                }
            }
        }

        if let Some(e) = final_err {
            return Err(e);
        }
        if panicked.load(Ordering::SeqCst) {
            let (idx, message) = first_panic
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or((0, "<panic details unavailable>".to_string()));
            return Err(SimError::BatchFailed(format!(
                "a sweep worker thread panicked at combination index {idx}: {message}"
            )));
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Bar, M5_SECS};

    fn int_spec(name: &str, min: f64, max: f64, step: f64, default: f64) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            kind: ParamKind::Int,
            min,
            max,
            step,
            default,
        }
    }

    fn float_spec(name: &str, min: f64, max: f64, step: f64, default: f64) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            kind: ParamKind::Float,
            min,
            max,
            step,
            default,
        }
    }

    // ---- Step 1: 列挙 -------------------------------------------------------

    #[test]
    fn candidate_count_for_an_integer_grid() {
        let spec = int_spec("entry", 10.0, 60.0, 2.0, 20.0);
        assert_eq!(candidate_count(&spec), 26);
    }

    #[test]
    fn candidate_count_for_a_float_grid_and_the_last_candidate_equals_max() {
        let spec = float_spec("mult", 0.5, 2.0, 0.5, 2.0);
        assert_eq!(candidate_count(&spec), 4);

        let last = combination_at(std::slice::from_ref(&spec), 3);
        match last.get("mult") {
            Some(ParamValue::Float(v)) => assert!((v - 2.0).abs() < FLOAT_GRID_TOLERANCE),
            other => panic!("expected Float, got {other:?}"),
        }
    }

    #[test]
    fn candidate_count_for_a_float_grid_includes_max_despite_accumulated_error() {
        // 0.3 - 0.1 は f64 では 0.19999999999999998 になる(累積誤差の確認)。
        let spec = float_spec("x", 0.1, 0.3, 0.1, 0.1);
        assert_eq!(spec.max - spec.min, 0.19999999999999998);
        assert_eq!(candidate_count(&spec), 3);

        let last = combination_at(std::slice::from_ref(&spec), 2);
        match last.get("x") {
            Some(ParamValue::Float(v)) => assert!((v - 0.3).abs() < FLOAT_GRID_TOLERANCE),
            other => panic!("expected Float, got {other:?}"),
        }
    }

    #[test]
    fn float_candidate_count_does_not_include_a_candidate_beyond_max_plus_tolerance() {
        // 300 は max + 1e-9 を超えるので候補に入らない: 0, 100, 200 の 3 個。
        let spec = float_spec("x", 0.0, 299.99999999, 100.0, 0.0);
        assert_eq!(candidate_count(&spec), 3);
    }

    #[test]
    fn float_candidate_count_includes_a_candidate_within_max_plus_tolerance() {
        // 0.003 は max + 1e-9 (= 0.0030000005) 以下なので候補に入る: 4 個。
        let spec = float_spec("x", 0.0, 0.0029999995, 0.001, 0.0);
        assert_eq!(candidate_count(&spec), 4);
    }

    #[test]
    fn zero_parameters_has_exactly_one_combination_which_is_empty() {
        assert_eq!(total_combinations(&[]).unwrap(), 1);
        assert_eq!(combination_at(&[], 0), ParamSet::new());
    }

    #[test]
    fn two_parameters_use_mixed_radix_with_the_last_as_least_significant_digit() {
        let specs = vec![
            int_spec("a", 0.0, 2.0, 1.0, 0.0), // 3 candidates: 0,1,2
            int_spec("b", 0.0, 3.0, 1.0, 0.0), // 4 candidates: 0,1,2,3
        ];
        assert_eq!(total_combinations(&specs).unwrap(), 12);

        let at0 = combination_at(&specs, 0);
        assert_eq!(at0["a"], ParamValue::Int(0));
        assert_eq!(at0["b"], ParamValue::Int(0));

        let at1 = combination_at(&specs, 1);
        assert_eq!(at1["a"], ParamValue::Int(0), "index 1: only b advances");
        assert_eq!(at1["b"], ParamValue::Int(1));

        let at4 = combination_at(&specs, 4);
        assert_eq!(at4["a"], ParamValue::Int(1), "index 4: a advances by one");
        assert_eq!(at4["b"], ParamValue::Int(0));
    }

    #[test]
    fn default_index_points_at_the_default_combination() {
        let specs = vec![
            int_spec("a", 0.0, 2.0, 1.0, 1.0),
            int_spec("b", 0.0, 3.0, 1.0, 2.0),
        ];
        let idx = default_index(&specs).unwrap();
        let combo = combination_at(&specs, idx);
        assert_eq!(combo["a"], ParamValue::Int(1));
        assert_eq!(combo["b"], ParamValue::Int(2));
    }

    #[test]
    fn default_index_matches_compiled_script_default_params() {
        let host = ScriptHost::new(&SimConfig::default());
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/donchian_sar.rhai"
        ))
        .expect("donchian_sar.rhai must exist");
        let script = host
            .compile(&source)
            .expect("spec 8.1 script must register");

        let idx = default_index(&script.params).unwrap();
        assert_eq!(combination_at(&script.params, idx), script.default_params());
    }

    #[test]
    fn total_combinations_rejects_a_script_whose_combination_count_exceeds_two_pow_63() {
        // 12 パラメータ x 1001 候補(min=0,max=1000,step=1) は 1001^12 ≈ 1e36 で、2^63 を大きく超える。
        let specs: Vec<ParamSpec> = (0..12)
            .map(|i| int_spec(&format!("p{i}"), 0.0, 1000.0, 1.0, 0.0))
            .collect();

        match total_combinations(&specs) {
            Err(SimError::Args(message)) => {
                assert!(
                    message.contains("narrow") && message.contains("coarser"),
                    "message must guide the operator to narrow ranges or use a coarser step, got {message:?}"
                );
            }
            other => panic!("expected Err(SimError::Args(_)), got {other:?}"),
        }
        assert!(
            matches!(default_index(&specs), Err(SimError::Args(_))),
            "default_index must reject the same way as total_combinations"
        );
        assert!(
            matches!(select_indices(&specs, 10, 42), Err(SimError::Args(_))),
            "select_indices must reject the same way as total_combinations"
        );
    }

    #[test]
    fn total_combinations_rejects_a_script_whose_combination_count_overflows_u128_without_panicking()
     {
        // 12 個の小数パラメータ x 10001 候補(min=0.0,max=10000.0,step=1.0) は 10001^12 ≈ 1e48 で、
        // u128 の範囲(約 3.4e38)も超える極端なケース。積が u128 を超えるほど大きいスクリプトでも、
        // 途中の積が 2^63 を超えた時点(5 個目の乗算)で拒否され、panic せず SimError::Args を返すことを確認する。
        let specs: Vec<ParamSpec> = (0..12)
            .map(|i| float_spec(&format!("p{i}"), 0.0, 10000.0, 1.0, 0.0))
            .collect();

        assert!(matches!(total_combinations(&specs), Err(SimError::Args(_))));
        assert!(matches!(default_index(&specs), Err(SimError::Args(_))));
        assert!(matches!(
            select_indices(&specs, 10, 42),
            Err(SimError::Args(_))
        ));
    }

    #[test]
    fn total_combinations_succeeds_when_the_combination_count_is_exactly_two_pow_63() {
        // 9 パラメータ x 128 候補(min=0,max=127,step=1) は 128^9 = 2^63 ちょうど。
        // 「超える」場合だけが拒否対象なので、これは上限以下として成功しなければならない。
        let specs: Vec<ParamSpec> = (0..9)
            .map(|i| int_spec(&format!("p{i}"), 0.0, 127.0, 1.0, 0.0))
            .collect();

        assert_eq!(total_combinations(&specs).unwrap(), 1u128 << 63);
        assert_eq!(
            default_index(&specs).unwrap(),
            0,
            "every spec's default is k=0, so the mixed-radix index is 0"
        );
        let indices =
            select_indices(&specs, 5, 42).expect("exactly 2^63 combinations must not be rejected");
        assert_eq!(indices.len(), 5);
        assert_eq!(indices[0], 0, "default index must come first");
    }

    #[test]
    fn total_combinations_rejects_when_one_more_parameter_pushes_the_count_to_two_pow_64() {
        // 上のテストの 9 パラメータ(128^9 = 2^63)に、候補 2 個のパラメータ(min=0,max=1,step=1)を
        // 1 つ足すと 128^9 * 2 = 2^64 になり、2^63 を超えるため 3 関数とも拒否する。
        let mut specs: Vec<ParamSpec> = (0..9)
            .map(|i| int_spec(&format!("p{i}"), 0.0, 127.0, 1.0, 0.0))
            .collect();
        specs.push(int_spec("extra", 0.0, 1.0, 1.0, 0.0));

        assert!(matches!(total_combinations(&specs), Err(SimError::Args(_))));
        assert!(matches!(default_index(&specs), Err(SimError::Args(_))));
        assert!(matches!(
            select_indices(&specs, 5, 42),
            Err(SimError::Args(_))
        ));
    }

    #[test]
    fn total_combinations_rejects_a_count_just_above_two_pow_63_but_below_two_pow_64() {
        // 128 候補 x 8 個 + 129 候補 x 1 個 = 128^8 * 129 = 2^63 + 2^56。
        // 2^63 < 積 < 2^64 なので、「u64 の桁あふれ」ではなく 2^63 の閾値そのものを区別できる。
        let mut specs: Vec<ParamSpec> = (0..8)
            .map(|i| int_spec(&format!("p{i}"), 0.0, 127.0, 1.0, 0.0))
            .collect();
        specs.push(int_spec("p8", 0.0, 128.0, 1.0, 0.0));

        assert!(matches!(total_combinations(&specs), Err(SimError::Args(_))));
        assert!(matches!(default_index(&specs), Err(SimError::Args(_))));
        assert!(matches!(
            select_indices(&specs, 5, 42),
            Err(SimError::Args(_))
        ));
    }

    #[test]
    fn default_index_at_the_two_pow_63_boundary_is_the_last_index() {
        // 9 パラメータ x 128 候補(2^63 通り)で default が全て最大値(127)なら、
        // 混合基数の index は最後の 2^63 - 1 になる。
        let specs: Vec<ParamSpec> = (0..9)
            .map(|i| int_spec(&format!("p{i}"), 0.0, 127.0, 1.0, 127.0))
            .collect();
        let last = (1u128 << 63) - 1;

        assert_eq!(default_index(&specs).unwrap(), last);
        let combo = combination_at(&specs, last);
        for spec in &specs {
            assert_eq!(combo[spec.name.as_str()], ParamValue::Int(127));
        }

        let indices =
            select_indices(&specs, 5, 42).expect("exactly 2^63 combinations must not be rejected");
        assert_eq!(indices.len(), 5);
        assert_eq!(indices[0], last, "default index must come first");
        assert!(indices.iter().all(|&i| i < (1u128 << 63)));
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 5, "no duplicates");
    }

    // ---- Step 2: 抽出 -------------------------------------------------------

    /// 候補が `n` 個の 1 パラメータ(`min=0, max=n-1, step=1, default=0`)。
    fn specs_with_total(n: u64) -> Vec<ParamSpec> {
        vec![int_spec("x", 0.0, (n - 1) as f64, 1.0, 0.0)]
    }

    #[test]
    fn selects_every_index_once_with_default_first_when_total_is_at_most_max_runs() {
        let specs = specs_with_total(5);
        let indices = select_indices(&specs, 10, 42).unwrap();

        assert_eq!(indices.len(), 5, "exactly one entry per combination");
        assert_eq!(indices[0], default_index(&specs).unwrap());
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4], "all 5 indices, no duplicates");
    }

    #[test]
    fn selects_every_index_once_with_a_non_zero_default_first_when_total_is_at_most_max_runs() {
        let specs = vec![int_spec("x", 0.0, 4.0, 1.0, 2.0)];
        let indices = select_indices(&specs, 10, 42).unwrap();

        assert_eq!(indices.len(), 5, "exactly one entry per combination");
        assert_eq!(indices[0], 2, "default (index 2) must come first");
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4], "all 5 indices, no duplicates");
    }

    #[test]
    fn sampling_with_a_middle_default_never_duplicates_or_leaves_the_index_range() {
        // default=13 / 27 候補: サンプルの j < default と j >= default の両分岐を通す。
        let specs = vec![int_spec("x", 0.0, 26.0, 1.0, 13.0)];
        for seed in 0..20u64 {
            let indices = select_indices(&specs, 26, seed).unwrap();

            assert_eq!(indices.len(), 26, "seed {seed}");
            assert_eq!(indices[0], 13, "seed {seed}: default first");
            assert!(indices.iter().all(|&i| i < 27), "seed {seed}: in range");
            let mut sorted = indices.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), 26, "seed {seed}: no duplicates");
        }
    }

    #[test]
    fn selects_max_runs_indices_with_default_first_when_total_is_one_more_than_max_runs() {
        // Review Focus 項目 5: 全組み合わせ数が max_runs より 1 だけ大きい場合。
        let specs = specs_with_total(27);
        let indices = select_indices(&specs, 26, 42).unwrap();

        assert_eq!(indices.len(), 26);
        assert_eq!(indices[0], default_index(&specs).unwrap());
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            26,
            "no duplicates among the 26 returned indices"
        );
    }

    #[test]
    fn same_seed_reproduces_the_same_selection_and_a_different_seed_differs() {
        let specs = specs_with_total(1000);
        let a1 = select_indices(&specs, 20, 1).unwrap();
        let a2 = select_indices(&specs, 20, 1).unwrap();
        assert_eq!(a1, a2, "same seed must reproduce the same selection");

        let b = select_indices(&specs, 20, 2).unwrap();
        assert_ne!(a1, b, "a different seed must change the selection");
    }

    #[test]
    fn max_runs_of_one_returns_only_the_default_index() {
        let specs = specs_with_total(1000);
        let indices = select_indices(&specs, 1, 42).unwrap();
        assert_eq!(indices, vec![default_index(&specs).unwrap()]);
    }

    #[test]
    fn zero_parameters_returns_a_single_combination() {
        // Review Focus 項目 5: パラメータが 0 個の場合。
        let indices = select_indices(&[], 26, 42).unwrap();
        assert_eq!(indices, vec![0]);
    }

    // ---- Step 3: 並列実行 ---------------------------------------------------

    /// 計画 Task 3 Step 3 の決定的な式から M5 の `Bar` を `n` 本作り、warmup なしの
    /// `Dataset` にする(script.rs のテストヘルパーと同じ構成。private なので複製する)。
    fn deterministic_dataset(n: usize) -> Arc<Dataset> {
        const SPREAD_MILLI: i64 = 10;
        let mid_closes: Vec<f64> = (0..n)
            .map(|i| 150.0 + 0.3 * (i as f64 * 0.07).sin() + 0.001 * i as f64)
            .collect();
        let close_milli: Vec<i64> = mid_closes
            .iter()
            .map(|&v| (v * 1000.0).round() as i64)
            .collect();
        let bars: Vec<Bar> = (0..n)
            .map(|i| {
                let close = close_milli[i];
                let open = if i == 0 {
                    close_milli[0]
                } else {
                    close_milli[i - 1]
                };
                let half = SPREAD_MILLI / 2;
                let hi = open.max(close);
                let lo = open.min(close);
                Bar {
                    open_time: i as i64 * M5_SECS,
                    bid_open: open - half,
                    bid_high: hi - half,
                    bid_low: lo - half,
                    bid_close: close - half,
                    ask_open: open + half,
                    ask_high: hi + half,
                    ask_low: lo + half,
                    ask_close: close + half,
                }
            })
            .collect();
        let from = bars[0].open_time;
        let to = bars[n - 1].open_time + M5_SECS;
        Arc::new(Dataset::new(bars, from, to, 0, 64).expect("deterministic dataset must build"))
    }

    fn donchian_sar_script(host: &ScriptHost) -> CompiledScript {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/donchian_sar.rhai"
        ))
        .expect("donchian_sar.rhai must exist");
        host.compile(&source)
            .expect("spec 8.1 script must register")
    }

    #[test]
    fn jobs_1_and_jobs_4_produce_the_same_set_of_run_records() {
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);
        let script = donchian_sar_script(&host);
        let dataset = deterministic_dataset(400);
        let benchmarks: Vec<Benchmark> = Vec::new();
        // entry: min=10,max=60,step=2 -> 26 candidates. max_runs=10 < 26 なのでサンプリングが働く。
        let indices = select_indices(&script.params, 10, 42).unwrap();
        assert_eq!(indices.len(), 10);

        let run_with = |jobs: usize| -> Vec<RunRecord> {
            let mut records = Vec::new();
            run_sweep(
                &dataset,
                &host,
                &script,
                &benchmarks,
                &cfg,
                &indices,
                jobs,
                &mut |record| {
                    records.push(record);
                    Ok(())
                },
            )
            .expect("run_sweep must succeed");
            // params をキーに並べ替えて比較する(到着順は jobs の数で変わる)。
            records.sort_by_key(|r| format!("{:?}", r.params));
            records
        };

        assert_eq!(run_with(1), run_with(4));
    }

    #[test]
    fn on_result_error_stops_further_on_result_calls_and_is_returned() {
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);
        let script = donchian_sar_script(&host);
        let dataset = deterministic_dataset(400);
        let benchmarks: Vec<Benchmark> = Vec::new();
        let indices = select_indices(&script.params, 8, 42).unwrap();
        assert_eq!(indices.len(), 8);

        let mut calls = 0usize;
        // jobs=1 にして、到着順と計算順を一致させ、呼び出し回数の検証を決定的にする。
        let result = run_sweep(
            &dataset,
            &host,
            &script,
            &benchmarks,
            &cfg,
            &indices,
            1,
            &mut |_record| {
                calls += 1;
                if calls == 3 {
                    Err(SimError::Args("stop after 3".to_string()))
                } else {
                    Ok(())
                }
            },
        );

        match result {
            Err(SimError::Args(message)) => assert_eq!(message, "stop after 3"),
            other => panic!("expected Err(SimError::Args(\"stop after 3\")), got {other:?}"),
        }
        assert_eq!(
            calls, 3,
            "on_result must not be called again after it returns Err"
        );
        assert!(calls < indices.len(), "not every index should be observed");
    }

    #[test]
    fn a_script_without_parameters_runs_exactly_once_with_an_empty_param_set() {
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);
        let script = host
            .compile("fn params() { #{} }\nfn on_bar(ctx, p) { 0 }\n")
            .expect("valid script");
        let dataset = deterministic_dataset(10);
        let benchmarks: Vec<Benchmark> = Vec::new();
        let indices = select_indices(&script.params, 26, 42).unwrap();

        let mut records = Vec::new();
        run_sweep(
            &dataset,
            &host,
            &script,
            &benchmarks,
            &cfg,
            &indices,
            1,
            &mut |record| {
                records.push(record);
                Ok(())
            },
        )
        .expect("run_sweep must succeed");

        assert_eq!(records.len(), 1, "on_result must be called exactly once");
        assert_eq!(records[0].params, ParamSet::new());
    }

    #[test]
    fn a_panicking_worker_is_reported_as_batch_failed_with_index_and_message() {
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);
        let mut script = host
            .compile("fn params() { #{} }\nfn on_bar(ctx, p) { 0 }\n")
            .expect("valid script");
        // step=0 の Int は登録検証(spec 8.1)を通らない値で、candidate_count の 0 除算により
        // combination_at が panic する。ワーカー panic を決定的に誘発するための細工。
        script.params.push(int_spec("bad", 0.0, 10.0, 0.0, 0.0));
        let dataset = deterministic_dataset(10);
        let benchmarks: Vec<Benchmark> = Vec::new();
        let indices = vec![7u128];

        for jobs in [1usize, 4] {
            let result = run_sweep(
                &dataset,
                &host,
                &script,
                &benchmarks,
                &cfg,
                &indices,
                jobs,
                &mut |_record| Ok(()),
            );
            match result {
                Err(SimError::BatchFailed(message)) => {
                    assert!(
                        message.contains("combination index 7"),
                        "jobs={jobs}: missing index in {message:?}"
                    );
                    assert!(
                        message.contains("divide by zero"),
                        "jobs={jobs}: missing panic message in {message:?}"
                    );
                }
                other => panic!("jobs={jobs}: expected Err(BatchFailed), got {other:?}"),
            }
        }
    }

    #[test]
    fn a_sweep_with_one_runtime_erroring_parameter_still_returns_ok_overall() {
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);
        // mode=0: 何もしない(常に Ok)。mode=1: period=2000(範囲外 1..=1000)で実行時エラー。
        let source = "fn params() { #{ mode: #{ min: 0, max: 1, step: 1, \"default\": 0 } } }\n\
                       fn on_bar(ctx, p) {\n\
                       if p.mode == 1 { let x = ctx.sma(\"M5\", 2000, 0); }\n\
                       0\n\
                       }\n";
        let script = host.compile(source).expect("valid script");
        let dataset = deterministic_dataset(10);
        let benchmarks: Vec<Benchmark> = Vec::new();
        let indices = select_indices(&script.params, 2, 42).unwrap();
        assert_eq!(indices.len(), 2, "mode has exactly 2 candidates (0 and 1)");

        let mut records = Vec::new();
        let result = run_sweep(
            &dataset,
            &host,
            &script,
            &benchmarks,
            &cfg,
            &indices,
            1,
            &mut |record| {
                records.push(record);
                Ok(())
            },
        );

        assert!(
            result.is_ok(),
            "run_sweep itself must return Ok even if one run is a script_error, got {result:?}"
        );
        assert_eq!(records.len(), 2);
        let ok_count = records
            .iter()
            .filter(|r| matches!(r.status, RunStatus::Ok))
            .count();
        let error_count = records
            .iter()
            .filter(|r| matches!(r.status, RunStatus::ScriptError { .. }))
            .count();
        assert_eq!(ok_count, 1, "mode=0 must simulate without error");
        assert_eq!(error_count, 1, "mode=1 must fail with a script_error");
    }
}
