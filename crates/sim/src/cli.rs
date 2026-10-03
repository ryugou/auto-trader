//! `auto-trader-sim` のサブコマンド定義と実行(spec 13 章、計画 Task 9)。
//!
//! `run`/`sweep` の処理順序は計画 Task 9 のとおり: 設定の読み込みと検証 → 引数の検証 →
//! DB 接続と `ensure_tables` → スクリプトの読み込みと `compile` → 足の読み込みと
//! `Dataset::new` → `missing_weekdays` の WARN → 基準値の計算 → `register_script` →
//! `create_batch` → 実行と `save_run` → `finish_batch` → 出力。バッチの行を作った後の失敗は、
//! すべて `finish_batch(Failed)` を試みてから終了コード 1 を返す(`fail_batch_and_exit`)。
//! `finish_batch(Failed)` 自体が失敗した場合は ERROR で記録し、`sim_batches` の行は
//! `running` のまま残る(spec 12 章: プロセスが強制終了した場合と同じ扱い)。
//!
//! ログは `tracing` で標準エラーへ、結果(表または JSON)は標準出力へ出す(spec 13 章)。
//! この分離のため `main.rs` は `tracing_subscriber` の writer を明示的に `stderr` にする
//! (`crates/app/src/main.rs` の初期化は既定の stdout のままだが、`auto-trader-sim` は
//! 結果を stdout でパイプ消費させる前提のコマンドであり、ログを混ぜてはいけないという
//! spec 13 章の明示的な要求があるため、ここでは既存パターンから意図的に逸脱する)。

use crate::benchmark::{self, Benchmark};
use crate::config::{self, SimConfig};
use crate::data;
use crate::engine::RunStatus;
use crate::error::SimError;
use crate::fetch::{self, GmoKlineClient};
use crate::script::{CompiledScript, ParamSet, ScriptHost};
use crate::series::{self, Dataset};
use crate::store::{self, BatchStatus, NewBatch};
use crate::sweep::{self, RunRecord, run_sweep};
use crate::types::{Bar, milli_to_pips};
use chrono::NaiveDate;
use clap::Parser;
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::time::Instant;
use uuid::Uuid;

/// `sweep` の上位件数の出力数(spec 13 章: 上位 10 件)。
const SWEEP_TOP_RUNS_LIMIT: i64 = 10;

/// `sweep --max-runs` の上限値(spec 13 章: 1 以上 1,000,000 以下)。
const MAX_RUNS_LIMIT: i64 = 1_000_000;

#[derive(Debug, clap::Parser)]
#[command(name = "auto-trader-sim")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// `migrations/` のマイグレーションを適用する(spec 13 章)。
    Migrate,
    /// 5.2 章の取得を `from` から `to` までの各日付(両端を含む)について行う。
    Backfill {
        #[arg(long)]
        from: NaiveDate,
        #[arg(long)]
        to: NaiveDate,
        #[arg(long)]
        json: bool,
    },
    /// `thetas_pips` の各値について、波の数・理論値・実質上限を出力する。
    Benchmark {
        #[arg(long)]
        from: Option<NaiveDate>,
        #[arg(long)]
        to: Option<NaiveDate>,
        #[arg(long)]
        json: bool,
    },
    /// スクリプトを登録し、シミュレーションを 1 回実行して保存する。
    Run {
        #[arg(long)]
        script: PathBuf,
        /// 指定しなかったパラメータは `default` を使う(spec 13 章)。
        #[arg(long)]
        params: Option<String>,
        #[arg(long)]
        from: Option<NaiveDate>,
        #[arg(long)]
        to: Option<NaiveDate>,
        #[arg(long)]
        json: bool,
    },
    /// パラメータ探索を実行して保存する(spec 11 章)。
    Sweep {
        #[arg(long)]
        script: PathBuf,
        #[arg(long)]
        from: Option<NaiveDate>,
        #[arg(long)]
        to: Option<NaiveDate>,
        /// 1 以上 [`MAX_RUNS_LIMIT`] 以下(spec 13 章)。
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=MAX_RUNS_LIMIT))]
        max_runs: u32,
        /// 省略時は 42(spec 13 章)。
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// 1 以上。省略時は設定値(`sim.jobs`)。0 は黙って 1 扱いにせず引数の誤りとする。
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        jobs: Option<u64>,
        #[arg(long)]
        json: bool,
    },
}

// ---------------------------------------------------------------------------
// エントリポイント
// ---------------------------------------------------------------------------

/// argv(プログラム名を含む)を解析して実行する。終了コードを返す。
///
/// clap の解析失敗(未知の引数、`--max-runs` の範囲外、必須引数の不足等)も、clap 既定の
/// 終了コード 2 ではなく spec 13 章の「引数の誤り」の終了コード 1 に合わせる(`--help`/
/// `--version` は成功として扱い 0 を返す)。`Cli::parse()` は失敗時にプロセスを直接
/// `std::process::exit` させてしまい、この変換ができないため `try_parse_from` を使う。
pub async fn run_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    match Cli::try_parse_from(args) {
        Ok(cli) => run(cli).await,
        Err(e) => {
            // clap がヘルプ・エラーメッセージを適切なストリーム(stdout/stderr)へ出力する。
            // 書き込みに失敗しても(例: 出力先が閉じている)握りつぶさず ERROR で記録する
            // (CLAUDE.md: エラーをログなしで握りつぶすことを禁止)。clap のエラー判定・終了
            // コードの決定自体は出力の成否と独立なので、ログの後も続行する。
            if let Err(print_err) = e.print() {
                tracing::error!(
                    error = %print_err,
                    "failed to print the clap help/usage error output to stdout/stderr"
                );
            }
            if e.exit_code() == 0 { 0 } else { 1 }
        }
    }
}

/// 設定の読み込みから結果の出力までを行う。終了コードを返す。
pub async fn run(cli: Cli) -> i32 {
    run_with_config_path(cli, &config::config_path()).await
}

/// `run` の内部実装。設定ファイルのパスを引数に取ることで、テストが `CONFIG_PATH` 環境変数を
/// 書き換えずに任意の設定ファイルを指定できる(計画 Task 9 確定事項 3)。`pub` なのは
/// `tests/cli_test.rs`(別クレート)がこの関数を直接呼ぶ必要があるため。
pub async fn run_with_config_path(cli: Cli, config_path: &Path) -> i32 {
    run_with_config_path_and_output(cli, config_path, &mut std::io::stdout()).await
}

/// `run_with_config_path` の結果出力先を差し替えられる版。結合テストが `Vec<u8>` で出力を
/// 捕捉して JSON を検証するために `pub`。出力の書き込みに失敗した場合(パイプが閉じた等)は
/// panic せず ERROR ログを出して終了コード 1 を返す。`sweep`/`run` ではその時点でバッチは既に
/// `completed` として確定済みのため、出力失敗でバッチの状態は変えない(結果は DB に残っており、
/// 出力だけが届かなかったことをログで運用者に伝える)。
pub async fn run_with_config_path_and_output(
    cli: Cli,
    config_path: &Path,
    out: &mut (dyn std::io::Write + Send),
) -> i32 {
    match cli.command {
        Command::Migrate => run_migrate(config_path).await,
        Command::Backfill { from, to, json } => {
            run_backfill(config_path, from, to, json, out).await
        }
        Command::Benchmark { from, to, json } => {
            run_benchmark(config_path, from, to, json, out).await
        }
        Command::Run {
            script,
            params,
            from,
            to,
            json,
        } => run_run(config_path, script, params, from, to, json, out).await,
        Command::Sweep {
            script,
            from,
            to,
            max_runs,
            seed,
            jobs,
            json,
        } => {
            run_sweep_cmd(
                config_path,
                script,
                from,
                to,
                max_runs,
                seed,
                jobs,
                json,
                out,
            )
            .await
        }
    }
}

/// 結果出力の書き込み失敗をログに残して終了コード 1 にする。
fn output_failed(e: std::io::Error) -> i32 {
    tracing::error!(
        error = %e,
        "failed to write the result to the output stream (e.g. the pipe was closed); \
         any database writes made before this point are already committed"
    );
    1
}

// ---------------------------------------------------------------------------
// --from / --to の省略時の規則(spec 13 章)
// ---------------------------------------------------------------------------

/// `--from` / `--to` の省略時の規則(spec 13 章)を適用して、評価期間 `[from, to)` を
/// エポック秒で返す。
///
/// `all_bars` が空の場合、`--from`/`--to` の省略可否によらず常に `SimError::Args` を返す
/// (空のデータから評価期間を組み立てる意味がなく、明示指定の場合でも後続の `Dataset::new`
/// がいずれ同じ理由で拒否するため、ここで早期に分かりやすいメッセージを出す)。
pub fn resolve_period(
    all_bars: &[Bar],
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    warmup_bars: usize,
) -> Result<(i64, i64), SimError> {
    if all_bars.is_empty() {
        return Err(SimError::Args(
            "no bars are loaded in sim_candles; run `backfill` first".to_string(),
        ));
    }

    let from_secs = match from {
        Some(d) => naive_date_to_utc_secs(d),
        None => series::earliest_from(all_bars, warmup_bars).ok_or_else(|| {
            SimError::Args(format!(
                "cannot determine the default --from: fewer than warmup_bars={warmup_bars} \
                 bars are loaded before any candidate start (have {} bars total); pass --from \
                 explicitly or run `backfill` to load more history",
                all_bars.len()
            ))
        })?,
    };
    let to_secs = match to {
        Some(d) => naive_date_to_utc_secs(d),
        None => {
            let last_bar = all_bars
                .last()
                .expect("checked all_bars is non-empty above");
            let last_date = chrono::DateTime::<chrono::Utc>::from_timestamp(last_bar.open_time, 0)
                .expect("Bar.open_time must be a valid UTC timestamp (data.rs invariant)")
                .date_naive();
            let next_date = last_date.succ_opt().expect(
                "date arithmetic must not overflow for real market data (far from NaiveDate::MAX)",
            );
            naive_date_to_utc_secs(next_date)
        }
    };

    if from_secs >= to_secs {
        return Err(SimError::Args(format!(
            "evaluation period [from, to) is empty or inverted: from={from_secs} to={to_secs} \
             (UTC epoch seconds); --from must be strictly before --to"
        )));
    }

    Ok((from_secs, to_secs))
}

/// `date` の UTC 0:00 をエポック秒にする。`NaiveDate::and_time` は失敗しない。
fn naive_date_to_utc_secs(date: NaiveDate) -> i64 {
    date.and_time(chrono::NaiveTime::MIN).and_utc().timestamp()
}

// ---------------------------------------------------------------------------
// 共通処理: 設定の読み込み・DB 接続・テーブル確認(`migrate` 以外の全サブコマンド)
// ---------------------------------------------------------------------------

struct Prepared {
    pool: PgPool,
    sim: SimConfig,
}

/// 設定の読み込みと検証 → DB 接続 → `ensure_tables` までを行う(spec 13 章の処理順序の
/// 共通部分)。失敗した場合はログを出し、終了コード(常に 1)を `Err` で返す。
async fn prepare(config_path: &Path) -> Result<Prepared, i32> {
    let settings = config::load(config_path).map_err(|e| {
        tracing::error!(
            config_path = %config_path.display(),
            error = %e,
            "failed to load or validate sim config"
        );
        1
    })?;
    let pool = data::connect(&settings.database_url).await.map_err(|e| {
        tracing::error!(error = %e, "failed to connect to database");
        1
    })?;
    data::ensure_tables(&pool).await.map_err(|e| {
        tracing::error!(error = %e, "required sim_* tables are missing");
        1
    })?;
    Ok(Prepared {
        pool,
        sim: settings.sim,
    })
}

/// バッチの行を作った後の失敗はすべてこれを経由する(spec 12 章)。`finish_batch(Failed)`
/// 自体の失敗は ERROR で記録するだけで、行は `running` のまま残る。
async fn fail_batch_and_exit(pool: &PgPool, batch_id: Uuid) -> i32 {
    if let Err(e) = store::finish_batch(pool, batch_id, BatchStatus::Failed).await {
        tracing::error!(
            batch_id = %batch_id,
            error = %e,
            "failed to mark sim_batches row as failed after an earlier failure; \
             the row is stuck in 'running' state and needs manual investigation"
        );
    }
    1
}

// ---------------------------------------------------------------------------
// テスト専用の差し替え点: `run`/`sweep` の実行・保存を関数引数のフックとして分離し、
// 結合テスト(`crates/sim/tests/cli_test.rs`)が「保存の失敗以外の理由
// (panic を含む)で中断した場合」「途中の保存が失敗した場合」の `failed` 遷移を再現できる
// ようにする(spec 12 章)。公開 API である `run`/`Cli`/`Command`/`resolve_period` の
// シグネチャにはこのフックを一切出さない。本番経路(`run_run`/`run_sweep_cmd`)は常にこの節の
// デフォルト値(`sweep::run_one` そのもの、`real_save_run()`)を渡すだけで、挙動は変わらない。
// ---------------------------------------------------------------------------

/// `run` が `sweep::run_one` の代わりに呼ぶ関数の型(テスト専用)。`sweep::run_one` と
/// シグネチャが一致するため、本番経路は関数ポインタとして `sweep::run_one` をそのまま渡せる
/// (ラッパー不要)。
#[doc(hidden)]
pub type RunOneFn = fn(
    &std::sync::Arc<Dataset>,
    &ScriptHost,
    &CompiledScript,
    &ParamSet,
    &[Benchmark],
    &SimConfig,
) -> RunRecord;

/// `run`/`sweep` が `store::save_run` の代わりに呼ぶ関数の型(テスト専用)。`store::save_run`
/// は `&PgPool`/`&RunRecord` を借用する async fn であり、素の関数ポインタでは借用の寿命を
/// シグネチャに表せないため、戻り値を `Pin<Box<dyn Future>>` にする。`Arc<dyn Fn + Send +
/// Sync>` にしているのは、sweep の「N 回保存に成功した後から失敗させる」テストがカウンタを
/// `move` キャプチャする必要があり、キャプチャできない素の関数ポインタでは表現できないため。
#[doc(hidden)]
pub type SaveRunFn = std::sync::Arc<
    dyn for<'a> Fn(
            &'a PgPool,
            Uuid,
            &'a RunRecord,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Uuid, SimError>> + Send + 'a>,
        > + Send
        + Sync,
>;

/// `SaveRunFn` の本番実装。`store::save_run` をそのまま呼ぶ。`run_run`/`run_sweep_cmd`(本番
/// 経路)と、保存だけを差し替えたいテストの両方から使う共通のデフォルト値。
#[doc(hidden)]
pub fn real_save_run() -> SaveRunFn {
    std::sync::Arc::new(|pool, batch_id, record| Box::pin(store::save_run(pool, batch_id, record)))
}

fn warn_missing_weekdays(dataset: &Dataset) {
    for date in dataset.missing_weekdays() {
        tracing::warn!(
            date = %date,
            "no bars for this UTC weekday within the evaluation period"
        );
    }
}

/// `--script` を読み込んで `compile` する(`run`/`sweep` 共通)。失敗はログを出して `Err(1)`。
fn read_and_compile(host: &ScriptHost, script_path: &Path) -> Result<CompiledScript, i32> {
    let source = std::fs::read_to_string(script_path).map_err(|e| {
        tracing::error!(script = %script_path.display(), error = %e, "failed to read script file");
        1
    })?;
    host.compile(&source).map_err(|e| {
        tracing::error!(script = %script_path.display(), error = %e, "script failed registration validation");
        1
    })
}

/// 足の読み込み → 評価期間の決定 → `Dataset::new` → `missing_weekdays` の WARN
/// (`benchmark`/`run`/`sweep` 共通)。失敗はログを出して `Err(1)`。
async fn build_dataset(
    prepared: &Prepared,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<(Dataset, i64, i64), i32> {
    let all_bars = data::load_bars(&prepared.pool, None).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load bars from sim_candles");
        1
    })?;
    let (from_secs, to_secs) = resolve_period(&all_bars, from, to, prepared.sim.warmup_bars)
        .map_err(|e| {
            tracing::error!(error = %e, "failed to resolve evaluation period");
            1
        })?;
    let dataset = Dataset::new(
        all_bars,
        from_secs,
        to_secs,
        prepared.sim.warmup_bars,
        prepared.sim.indicator_cache_mb,
    )
    .map_err(|e| {
        tracing::error!(error = %e, "failed to build dataset for the evaluation period");
        1
    })?;
    warn_missing_weekdays(&dataset);
    Ok((dataset, from_secs, to_secs))
}

fn compute_benchmarks(dataset: &Dataset, sim: &SimConfig) -> Vec<Benchmark> {
    sim.thetas_pips
        .iter()
        .map(|&theta| benchmark::compute(dataset.eval_bars(), theta))
        .collect()
}

/// `run`/`sweep` が `create_batch` の前に必要とする評価入力一式。
struct EvalInputs {
    dataset: Dataset,
    from_secs: i64,
    to_secs: i64,
    bar_count: i32,
    benchmarks: Vec<Benchmark>,
}

/// `build_dataset` に続けて `bar_count` の i32 変換と基準値の計算を行う(計画 Task 9 の順序)。
async fn load_eval_inputs(
    prepared: &Prepared,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<EvalInputs, i32> {
    let (dataset, from_secs, to_secs) = build_dataset(prepared, from, to).await?;
    let bar_count = i32::try_from(dataset.eval_bars().len()).map_err(|e| {
        tracing::error!(
            error = %e,
            eval_bars = dataset.eval_bars().len(),
            "evaluation period bar count does not fit sim_batches.bar_count (i32)"
        );
        1
    })?;
    let benchmarks = compute_benchmarks(&dataset, &prepared.sim);
    Ok(EvalInputs {
        dataset,
        from_secs,
        to_secs,
        bar_count,
        benchmarks,
    })
}

/// `--script` のファイル名から拡張子を除いた名前(spec 12 章: `sim_scripts.name`)。
fn script_name_from_path(path: &Path) -> Result<String, SimError> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            SimError::Args(format!(
                "cannot derive a script name from path {} (expected a file name with a valid \
                 UTF-8 stem)",
                path.display()
            ))
        })
}

/// `--params` の生の JSON テキストを `resolve_params` に渡せる形にする。省略時は空(すべて
/// `default`)。JSON として不正、またはオブジェクトでない場合は `SimError::Args`。
fn parse_given_params(
    raw: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, SimError> {
    let Some(raw) = raw else {
        return Ok(serde_json::Map::new());
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(other) => Err(SimError::Args(format!(
            "--params must be a JSON object, got {other}"
        ))),
        Err(e) => Err(SimError::Args(format!(
            "--params is not valid JSON: {e} (input={raw:?})"
        ))),
    }
}

fn run_batch_config_json(sim: &SimConfig) -> serde_json::Value {
    serde_json::json!({
        "warmup_bars": sim.warmup_bars,
        "protective_stop_pips": sim.protective_stop_pips,
        "thetas_pips": sim.thetas_pips,
        "max_operations_per_bar": sim.max_operations_per_bar,
        "max_operations_per_run": sim.max_operations_per_run,
    })
}

fn sweep_batch_config_json(sim: &SimConfig, max_runs: usize, seed: u64) -> serde_json::Value {
    let mut value = run_batch_config_json(sim);
    value["max_runs"] = serde_json::json!(max_runs);
    value["seed"] = serde_json::json!(seed);
    value
}

/// `--jobs`(省略時は設定値)を実行環境・実行件数に合わせて丸める(spec 11 章: 「スレッド数
/// は、`jobs`、実行件数、実行環境で利用可能な並列数(`std::thread::available_parallelism`)
/// のうち最小の値とし、1 を下回らない」)。
///
/// `requested`・`run_count` のどちらも実行件数・設定値から来る値で上限がないため、
/// `available` で頭打ちにしないと(例: `--max-runs 1000000 --jobs 1000000`)
/// `run_sweep` が実行環境のコア数を大きく超える OS スレッドを作ろうとして panic しうる。
/// `available` は呼び出し側が `std::thread::available_parallelism()` から求めて渡す
/// (取得失敗時は呼び出し側が WARN を出して `1` を渡す契約)。
fn effective_jobs(requested: usize, run_count: usize, available: usize) -> usize {
    requested.min(run_count).min(available).max(1)
}

fn format_utc_rfc3339(epoch_secs: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch_secs, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| {
            tracing::warn!(
                epoch_secs,
                "epoch seconds could not be converted to RFC3339; falling back to the raw epoch-seconds representation"
            );
            epoch_secs.to_string()
        })
}

// ---------------------------------------------------------------------------
// migrate
// ---------------------------------------------------------------------------

async fn run_migrate(config_path: &Path) -> i32 {
    let settings = match config::load(config_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                config_path = %config_path.display(),
                error = %e,
                "failed to load or validate sim config"
            );
            return 1;
        }
    };
    let pool = match data::connect(&settings.database_url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to connect to database");
            return 1;
        }
    };
    match sqlx::migrate!("../../migrations").run(&pool).await {
        Ok(()) => {
            tracing::info!("sim_candles/sim_scripts/sim_batches/sim_runs migrations applied");
            0
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to apply migrations");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// backfill
// ---------------------------------------------------------------------------

async fn run_backfill(
    config_path: &Path,
    from: NaiveDate,
    to: NaiveDate,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    // 引数の誤りは DB 接続前に判定する(計画 Task 9: 引数の検証 → DB 接続)。
    if from > to {
        tracing::error!(from = %from, to = %to, "--from must not be after --to");
        return 1;
    }
    let min_date = fetch::gmo_kline_min_date();
    if from < min_date {
        // この日付より前は GMO API が空の失敗応答を返し続けるだけで、日付ごとに
        // retry_delays(2s/4s/8s)を無駄に消費する(spec 13 章)。DB 接続前に拒否する。
        let message = format!(
            "--from={from} is earlier than the oldest date the GMO kline API accepts \
             ({min_date}); pass --from {min_date} or a later date"
        );
        tracing::error!(from = %from, min_date = %min_date, "{message}");
        return 1;
    }
    let prepared = match prepare(config_path).await {
        Ok(p) => p,
        Err(code) => return code,
    };

    // クライアントは 1 つだけ作って全日で使い回す: リクエスト間隔(1 秒)の共有状態が
    // クライアント内にあるため、日ごとに作り直すと間隔が保たれない。
    let client = GmoKlineClient::new(&prepared.sim.gmo_public_base_url);

    // `fetch::backfill` は一部失敗のとき件数を捨てて `FetchIncomplete` だけを返すため、
    // 範囲全体を 1 回で呼ぶと件数が失われる。1 日ずつ呼んで自前で集計する。
    let mut total = fetch::BackfillReport {
        days: 0,
        saved: 0,
        one_sided: 0,
        invalid: 0,
        failed: Vec::new(),
    };
    let mut date = from;
    loop {
        match fetch::backfill(&prepared.pool, &client, date, date).await {
            Ok(r) => {
                total.days += r.days;
                total.saved += r.saved;
                total.one_sided += r.one_sided;
                total.invalid += r.invalid;
                total.failed.extend(r.failed);
            }
            // fetch.rs の実装上、BID/ASK のどちらかの取得に失敗した日は結合・保存を行わず
            // failed に積むだけなので、その日の saved/one_sided/invalid は必ず 0。
            // したがって days を 1 加えて failed を足すだけで集計は正確になる。
            Err(SimError::FetchIncomplete(f)) => {
                total.days += 1;
                total.failed.extend(f);
            }
            // 保存失敗など。この日以降は処理しない(再実行で埋まる)。
            Err(e) => {
                tracing::error!(
                    date = %date,
                    from = %from,
                    to = %to,
                    days = total.days,
                    saved = total.saved,
                    one_sided = total.one_sided,
                    invalid = total.invalid,
                    failed = ?total.failed,
                    error = %e,
                    "backfill aborted by a non-fetch error; counts above cover only the dates \
                     processed before this one"
                );
                return 1;
            }
        }
        if date == to {
            break;
        }
        // `succ_opt` が None になるのは NaiveDate::MAX のみで、その場合 `date >= to` なので終了。
        match date.succ_opt() {
            Some(next) => date = next,
            None => break,
        }
    }

    if let Err(e) = print_backfill(out, json, &total) {
        return output_failed(e);
    }
    if !total.failed.is_empty() {
        tracing::error!(
            from = %from,
            to = %to,
            failed = ?total.failed,
            "backfill: some dates failed to fetch after retries; rerun the same range to \
             fill the gaps (already-saved dates are unaffected)"
        );
        return 1;
    }
    0
}

/// `backfill` の `--json` 出力を組み立てる(`print_backfill` から分離: 戻り値として
/// `serde_json::Value` を直接返すことで、出力先なしで内容を単体テストできる)。
fn backfill_json(report: &fetch::BackfillReport) -> serde_json::Value {
    serde_json::json!({
        "days": report.days,
        "saved": report.saved,
        "one_sided": report.one_sided,
        "invalid": report.invalid,
        "failed": report.failed,
    })
}

fn print_backfill(
    out: &mut dyn std::io::Write,
    json: bool,
    report: &fetch::BackfillReport,
) -> std::io::Result<()> {
    if json {
        writeln!(out, "{}", backfill_json(report))?;
    } else {
        print_kv_table(
            out,
            &[
                ("days".to_string(), report.days.to_string()),
                ("saved".to_string(), report.saved.to_string()),
                ("one_sided".to_string(), report.one_sided.to_string()),
                ("invalid".to_string(), report.invalid.to_string()),
                ("failed".to_string(), report.failed.join(", ")),
            ],
        )?;
    }
    // `out` の Drop 時の暗黙の flush はエラーを握りつぶすため、ここで明示的に flush して
    // 書き込み失敗を拾う。失敗はこの関数の `Err` として `output_failed` 経路(ERROR ログ +
    // 終了コード 1)に流れる。
    out.flush()
}

// ---------------------------------------------------------------------------
// benchmark
// ---------------------------------------------------------------------------

async fn run_benchmark(
    config_path: &Path,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    let prepared = match prepare(config_path).await {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (dataset, _, _) = match build_dataset(&prepared, from, to).await {
        Ok(d) => d,
        Err(code) => return code,
    };
    let benchmarks = compute_benchmarks(&dataset, &prepared.sim);
    match print_benchmark(out, &benchmarks, json) {
        Ok(()) => 0,
        Err(e) => output_failed(e),
    }
}

/// `benchmark` の `--json` 出力を組み立てる(`print_benchmark` から分離した理由は
/// `backfill_json` と同じ: stdout を奪わずに内容を単体テストできるようにするため)。
fn benchmark_json(benchmarks: &[Benchmark]) -> serde_json::Value {
    let by_theta: std::collections::BTreeMap<String, serde_json::Value> = benchmarks
        .iter()
        .map(|b| {
            (
                b.theta_pips.to_string(),
                serde_json::json!({
                    "leg_count": b.legs.len(),
                    "ideal_pips": milli_to_pips(b.ideal_milli),
                    "realizable_pips": milli_to_pips(b.realizable_milli),
                }),
            )
        })
        .collect();
    serde_json::json!({ "by_theta": by_theta })
}

fn print_benchmark(
    out: &mut dyn std::io::Write,
    benchmarks: &[Benchmark],
    json: bool,
) -> std::io::Result<()> {
    if json {
        writeln!(out, "{}", benchmark_json(benchmarks))?;
    } else {
        let rows: Vec<Vec<String>> = benchmarks
            .iter()
            .map(|b| {
                vec![
                    b.theta_pips.to_string(),
                    b.legs.len().to_string(),
                    format!("{:.2}", milli_to_pips(b.ideal_milli)),
                    format!("{:.2}", milli_to_pips(b.realizable_milli)),
                ]
            })
            .collect();
        print_columns(
            out,
            &["theta_pips", "leg_count", "ideal_pips", "realizable_pips"],
            &rows,
        )?;
    }
    // `out` の Drop 時の暗黙の flush はエラーを握りつぶすため、明示的に flush する。失敗は
    // `output_failed` 経路(ERROR ログ + 終了コード 1)に流れる。
    out.flush()
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

async fn run_run(
    config_path: &Path,
    script_path: PathBuf,
    params_raw: Option<String>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    // 本番経路は常に実物の `sweep::run_one`(シグネチャが `RunOneFn` と一致するため関数
    // ポインタとしてそのまま渡せる)と `real_save_run()` を使う。差し替えはテスト専用。
    run_run_with_hooks(
        config_path,
        script_path,
        params_raw,
        from,
        to,
        json,
        out,
        sweep::run_one,
        real_save_run(),
    )
    .await
}

/// `run_run` の内部実装。`sweep::run_one`/`store::save_run` を引数で差し替え可能にしてある。
///
/// テスト専用: `crates/sim/tests/cli_test.rs` が、保存の失敗以外の理由(`run_one` の panic を
/// 含む)で中断した場合に spec 12 章どおり `failed` へ遷移することを検証するために直接呼ぶ。
/// 本番経路(`run_run`)は常にこの関数へ実物の `sweep::run_one`/`real_save_run()` を渡すだけ
/// であり、挙動は変わらない。外部から使わないこと。
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn run_run_with_hooks(
    config_path: &Path,
    script_path: PathBuf,
    params_raw: Option<String>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    json: bool,
    out: &mut dyn std::io::Write,
    run_one_fn: RunOneFn,
    save_run_fn: SaveRunFn,
) -> i32 {
    // `--params` の JSON 構文は引数の検証として DB 接続前に行う。パラメータ名・値の検証
    // (`resolve_params`)はスクリプトの `params()` が必要なので compile 後。
    let given = match parse_given_params(params_raw.as_deref()) {
        Ok(g) => g,
        Err(e) => {
            tracing::error!(error = %e, "invalid --params");
            return 1;
        }
    };
    let prepared = match prepare(config_path).await {
        Ok(p) => p,
        Err(code) => return code,
    };

    let host = ScriptHost::new(&prepared.sim);
    let compiled = match read_and_compile(&host, &script_path) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let params = match compiled.resolve_params(&given) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "invalid --params");
            return 1;
        }
    };

    let EvalInputs {
        dataset,
        from_secs,
        to_secs,
        bar_count,
        benchmarks,
    } = match load_eval_inputs(&prepared, from, to).await {
        Ok(i) => i,
        Err(code) => return code,
    };

    let script_name = match script_name_from_path(&script_path) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "cannot derive script name");
            return 1;
        }
    };
    let script_id = match store::register_script(
        &prepared.pool,
        &script_name,
        &compiled,
        "human",
        None,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to register script");
            return 1;
        }
    };
    let batch_id = match store::create_batch(
        &prepared.pool,
        &NewBatch {
            script_id,
            period_from: from_secs,
            period_to: to_secs,
            bar_count,
            total_runs: 1,
            config: run_batch_config_json(&prepared.sim),
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to create sim_batches row");
            return 1;
        }
    };

    // `run_one_fn` を `spawn_blocking` 上で動かす: 同期の(CPU バウンドな)シミュレーションを
    // tokio のワーカースレッド上で直接動かすとランタイムをブロックするだけでなく、panic した
    // 場合にプロセスごと終了コード 101 で落ちてしまい、`sim_batches` の行が `running` のまま
    // 残る(spec 12 章違反)。`spawn_blocking` でラップすることで、panic は `JoinError` として
    // 戻り値に現れるため、既存の `fail_batch_and_exit` 経路に流せる(`run_sweep_cmd` が
    // `run_sweep` を `spawn_blocking` でラップしている箇所と同じパターン)。
    //
    // `host`/`compiled`/`params`/`benchmarks` はこの呼び出し後どこでも使わないため、
    // そのまま `move` する。`prepared.sim` は `prepared`(`&prepared.pool` を後で使う)を
    // 丸ごと move できないので `Clone` して渡す。
    let dataset = std::sync::Arc::new(dataset);
    let sim_cfg = prepared.sim.clone();
    let start = Instant::now();
    let run_result = tokio::task::spawn_blocking(move || {
        run_one_fn(&dataset, &host, &compiled, &params, &benchmarks, &sim_cfg)
    })
    .await;
    let record = match run_result {
        Ok(record) => record,
        Err(join_err) => {
            tracing::error!(
                batch_id = %batch_id,
                error = %join_err,
                "run_one panicked while executing the simulation; the process did not crash, \
                 but no sim_runs row was saved (spec 12 章: 保存の失敗以外の理由で中断した \
                 場合も failed にする)"
            );
            return fail_batch_and_exit(&prepared.pool, batch_id).await;
        }
    };
    let save_result = save_run_fn(&prepared.pool, batch_id, &record).await;
    let elapsed = start.elapsed().as_secs_f64();

    if let Err(e) = save_result {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to save sim_runs row");
        return fail_batch_and_exit(&prepared.pool, batch_id).await;
    }
    if let Err(e) = store::finish_batch(&prepared.pool, batch_id, BatchStatus::Completed).await {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to mark sim_batches row as completed");
        return fail_batch_and_exit(&prepared.pool, batch_id).await;
    }

    // バッチは completed で確定済み。出力の失敗ではバッチの状態を変えない。
    if let Err(e) = print_run(out, &record, elapsed, script_id, batch_id, json) {
        return output_failed(e);
    }
    match record.status {
        RunStatus::Ok => 0,
        RunStatus::ScriptError { .. } => 1,
    }
}

/// `run` の `--json` 出力を組み立てる(`print_run` から分離した理由は `backfill_json` と同じ)。
fn run_json(
    record: &RunRecord,
    elapsed: f64,
    script_id: Uuid,
    batch_id: Uuid,
) -> serde_json::Value {
    let (status, error) = run_status_and_error_message(record);

    let mut obj = serde_json::json!({
        "status": status,
        "elapsed_secs": elapsed,
        "script_id": script_id,
        "batch_id": batch_id,
    });
    if let Some(err) = &error {
        obj["error"] = serde_json::Value::String(err.clone());
    }
    if let Some(metrics) = &record.metrics {
        obj["metrics"] = serde_json::to_value(metrics)
            .expect("Metrics only contains finite numbers/strings and must serialize");
    }
    obj
}

fn run_status_and_error_message(record: &RunRecord) -> (&'static str, Option<String>) {
    match &record.status {
        RunStatus::Ok => ("ok", None),
        RunStatus::ScriptError { open_time, message } => (
            "script_error",
            Some(format!("{} {message}", format_utc_rfc3339(*open_time))),
        ),
    }
}

fn print_run(
    out: &mut dyn std::io::Write,
    record: &RunRecord,
    elapsed: f64,
    script_id: Uuid,
    batch_id: Uuid,
    json: bool,
) -> std::io::Result<()> {
    if json {
        writeln!(out, "{}", run_json(record, elapsed, script_id, batch_id))?;
        // `out` の Drop 時の暗黙の flush はエラーを握りつぶすため、明示的に flush する。失敗は
        // `output_failed` 経路(ERROR ログ + 終了コード 1)に流れる。
        return out.flush();
    }
    let (status, error) = run_status_and_error_message(record);

    let mut rows = vec![
        ("status".to_string(), status.to_string()),
        ("elapsed_secs".to_string(), format!("{elapsed:.2}")),
        ("script_id".to_string(), script_id.to_string()),
        ("batch_id".to_string(), batch_id.to_string()),
    ];
    if let Some(err) = &error {
        rows.push(("error".to_string(), err.clone()));
    }
    if let Some(metrics) = &record.metrics {
        rows.push((
            "total_pips".to_string(),
            format!("{:.2}", metrics.total_pips),
        ));
        rows.push(("trade_count".to_string(), metrics.trade_count.to_string()));
        rows.push(("win_rate".to_string(), format!("{:.4}", metrics.win_rate)));
        rows.push((
            "max_drawdown_pips".to_string(),
            format!("{:.2}", metrics.max_drawdown_pips),
        ));
        rows.push((
            "time_in_market".to_string(),
            format!("{:.4}", metrics.time_in_market),
        ));
        rows.push((
            "protective_stop_count".to_string(),
            metrics.protective_stop_count.to_string(),
        ));
    }
    print_kv_table(out, &rows)?;
    // `out` の Drop 時の暗黙の flush はエラーを握りつぶすため、明示的に flush する。失敗は
    // `output_failed` 経路(ERROR ログ + 終了コード 1)に流れる。
    out.flush()
}

// ---------------------------------------------------------------------------
// sweep
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_sweep_cmd(
    config_path: &Path,
    script_path: PathBuf,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    max_runs: u32,
    seed: u64,
    jobs: Option<u64>,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    // 本番経路は常に `real_save_run()`(実物の `store::save_run`)を使う。差し替えはテスト専用。
    run_sweep_cmd_with_hooks(
        config_path,
        script_path,
        from,
        to,
        max_runs,
        seed,
        jobs,
        json,
        out,
        real_save_run(),
    )
    .await
}

/// `run_sweep_cmd` の内部実装。`store::save_run` を引数で差し替え可能にしてある。
///
/// テスト専用: `crates/sim/tests/cli_test.rs` が、sweep の途中の保存が失敗した場合に
/// spec 12 章どおり `failed` へ遷移し、保存済みの `sim_runs` が全件数より少ないことを
/// 検証するために直接呼ぶ。本番経路(`run_sweep_cmd`)は常にこの関数へ `real_save_run()` を
/// 渡すだけであり、挙動は変わらない。外部から使わないこと。
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn run_sweep_cmd_with_hooks(
    config_path: &Path,
    script_path: PathBuf,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    max_runs: u32,
    seed: u64,
    jobs: Option<u64>,
    json: bool,
    out: &mut dyn std::io::Write,
    save_run_fn: SaveRunFn,
) -> i32 {
    // clap で 1 以上に制限済みだが、usize への変換失敗(32bit 環境等)も黙って丸めない。
    let jobs_override = match jobs.map(usize::try_from).transpose() {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(error = %e, "--jobs does not fit in usize on this platform");
            return 1;
        }
    };
    let prepared = match prepare(config_path).await {
        Ok(p) => p,
        Err(code) => return code,
    };

    let host = ScriptHost::new(&prepared.sim);
    let compiled = match read_and_compile(&host, &script_path) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let EvalInputs {
        dataset,
        from_secs,
        to_secs,
        bar_count,
        benchmarks,
    } = match load_eval_inputs(&prepared, from, to).await {
        Ok(i) => i,
        Err(code) => return code,
    };

    let max_runs_usize = max_runs as usize;
    // 全組み合わせ数が 2^63 を超えるスクリプトはここで拒否する(spec 11 章)。register_script /
    // create_batch より前に判定し、拒否時に sim_batches の行を作らない。
    let indices = match sweep::select_indices(&compiled.params, max_runs_usize, seed) {
        Ok(i) => i,
        Err(e) => {
            tracing::error!(error = %e, "failed to select sweep parameter combinations");
            return 1;
        }
    };
    let indices_len = indices.len();
    let total_runs = match i32::try_from(indices_len) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, indices_len, "selected run count does not fit sim_batches.total_runs (i32)");
            return 1;
        }
    };

    let script_name = match script_name_from_path(&script_path) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "cannot derive script name");
            return 1;
        }
    };
    let script_id = match store::register_script(
        &prepared.pool,
        &script_name,
        &compiled,
        "human",
        None,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to register script");
            return 1;
        }
    };
    let batch_id = match store::create_batch(
        &prepared.pool,
        &NewBatch {
            script_id,
            period_from: from_secs,
            period_to: to_secs,
            bar_count,
            total_runs,
            config: sweep_batch_config_json(&prepared.sim, max_runs_usize, seed),
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to create sim_batches row");
            return 1;
        }
    };

    let requested_jobs = jobs_override.unwrap_or(prepared.sim.jobs);
    // `available_parallelism()` の失敗(例: サンドボックス環境でのリソース問い合わせ拒否)は
    // 実行自体を止める理由にならないため、WARN に落として `1`(spec 11 章の下限)で進める。
    let available = match std::thread::available_parallelism() {
        Ok(n) => n.get(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to determine available_parallelism; assuming 1 for --jobs clamping"
            );
            1
        }
    };
    let jobs = effective_jobs(requested_jobs, indices_len, available);
    if jobs < requested_jobs {
        tracing::info!(
            requested_jobs,
            run_count = indices_len,
            available,
            jobs,
            "clamping --jobs down to the minimum of the requested value, the run count, and \
             available_parallelism to avoid spawning unused or oversubscribed worker threads"
        );
    }
    let dataset = std::sync::Arc::new(dataset);
    let sim_cfg = prepared.sim.clone();
    let pool_for_saves = prepared.pool.clone();
    let rt_handle = tokio::runtime::Handle::current();

    // `run_sweep` は同期関数で、ワーカースレッドの完了結果を呼び出し元のスレッド上で
    // `on_result` に直列で渡す(sweep.rs の設計)。`on_result` 内で `save_run_fn`
    // (async)を呼ぶ必要があるため、`run_sweep` 全体を `spawn_blocking` 上で動かし、
    // ブロッキングスレッド上でだけ `Handle::block_on` を使う(tokio のランタイムを
    // 駆動しているワーカースレッド上で `block_on` すると panic するため、async
    // ワーカースレッドの外側である `spawn_blocking` のスレッドに退避する)。
    let start = Instant::now();
    let sweep_result: Result<(), SimError> = tokio::task::spawn_blocking(move || {
        run_sweep(
            &dataset,
            &host,
            &compiled,
            &benchmarks,
            &sim_cfg,
            &indices,
            jobs,
            &mut |record: RunRecord| {
                rt_handle
                    .block_on(save_run_fn(&pool_for_saves, batch_id, &record))
                    .map(|_| ())
            },
        )
    })
    .await
    .unwrap_or_else(|join_err| {
        Err(SimError::BatchFailed(format!(
            "sweep worker task panicked outside run_sweep's own panic handling: {join_err}"
        )))
    });
    let elapsed = start.elapsed().as_secs_f64();

    if let Err(e) = sweep_result {
        tracing::error!(batch_id = %batch_id, error = %e, "sweep execution failed");
        return fail_batch_and_exit(&prepared.pool, batch_id).await;
    }
    if let Err(e) = store::finish_batch(&prepared.pool, batch_id, BatchStatus::Completed).await {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to mark sim_batches row as completed");
        return fail_batch_and_exit(&prepared.pool, batch_id).await;
    }

    let top = match store::top_runs(&prepared.pool, batch_id, SWEEP_TOP_RUNS_LIMIT).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(batch_id = %batch_id, error = %e, "failed to load top runs for output");
            return 1;
        }
    };
    // バッチは completed で確定済み。出力の失敗ではバッチの状態を変えない。
    match print_sweep(out, &top, elapsed, script_id, batch_id, indices_len, json) {
        Ok(()) => 0,
        Err(e) => output_failed(e),
    }
}

/// `sweep` の `--json` 出力を組み立てる(`print_sweep` から分離した理由は `backfill_json` と
/// 同じ)。
fn sweep_json(
    top: &[store::RunSummary],
    elapsed: f64,
    script_id: Uuid,
    batch_id: Uuid,
    total_runs: usize,
) -> serde_json::Value {
    let top_json: Vec<serde_json::Value> = top
        .iter()
        .map(|r| {
            serde_json::json!({
                "params": r.params,
                "total_pips": r.total_pips,
                "trade_count": r.trade_count,
                "metrics": r.metrics,
            })
        })
        .collect();
    serde_json::json!({
        "elapsed_secs": elapsed,
        "script_id": script_id,
        "batch_id": batch_id,
        "total_runs": total_runs,
        "top_runs": top_json,
    })
}

fn print_sweep(
    out: &mut dyn std::io::Write,
    top: &[store::RunSummary],
    elapsed: f64,
    script_id: Uuid,
    batch_id: Uuid,
    total_runs: usize,
    json: bool,
) -> std::io::Result<()> {
    if json {
        writeln!(
            out,
            "{}",
            sweep_json(top, elapsed, script_id, batch_id, total_runs)
        )?;
        // `out` の Drop 時の暗黙の flush はエラーを握りつぶすため、明示的に flush する。失敗は
        // `output_failed` 経路(ERROR ログ + 終了コード 1)に流れる。
        return out.flush();
    }
    print_kv_table(
        out,
        &[
            ("elapsed_secs".to_string(), format!("{elapsed:.2}")),
            ("script_id".to_string(), script_id.to_string()),
            ("batch_id".to_string(), batch_id.to_string()),
            ("total_runs".to_string(), total_runs.to_string()),
        ],
    )?;
    let rows: Vec<Vec<String>> = top
        .iter()
        .enumerate()
        .map(|(i, r)| {
            vec![
                (i + 1).to_string(),
                format!("{:.2}", r.total_pips),
                r.trade_count.to_string(),
                r.params.to_string(),
            ]
        })
        .collect();
    print_columns(out, &["rank", "total_pips", "trade_count", "params"], &rows)?;
    // `out` の Drop 時の暗黙の flush はエラーを握りつぶすため、明示的に flush する。失敗は
    // `output_failed` 経路(ERROR ログ + 終了コード 1)に流れる。
    out.flush()
}

// ---------------------------------------------------------------------------
// 出力ヘルパー(新規の表描画クレートを追加しない。既存依存で足りるため: 計画 Global
// Constraints「ルートの Cargo.toml は変更不要なはず」)
// ---------------------------------------------------------------------------

fn print_kv_table(out: &mut dyn std::io::Write, rows: &[(String, String)]) -> std::io::Result<()> {
    let width = rows
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    for (k, v) in rows {
        writeln!(out, "{k:<width$}  {v}")?;
    }
    Ok(())
}

fn print_columns(
    out: &mut dyn std::io::Write,
    headers: &[&str],
    rows: &[Vec<String>],
) -> std::io::Result<()> {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: &[String]| -> String {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect::<Vec<_>>()
            .join("  ")
    };
    let header_cells: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    writeln!(out, "{}", line(&header_cells))?;
    for row in rows {
        writeln!(out, "{}", line(row))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar_at(open_time: i64) -> Bar {
        Bar {
            open_time,
            bid_open: 0,
            bid_high: 0,
            bid_low: 0,
            bid_close: 0,
            ask_open: 0,
            ask_high: 0,
            ask_low: 0,
            ask_close: 0,
        }
    }

    /// series.rs のテストヘルパーと同じ構成(private なのでここで複製する)。
    fn utc_secs(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, 0)
            .unwrap()
            .and_utc()
            .timestamp()
    }

    const M5: i64 = 300;

    #[test]
    fn from_omitted_resolves_to_the_next_midnight_after_the_warmup_boundary_bar() {
        // bars[4] = 2024-01-03 21:55、bars[5] = 2024-01-03 22:20 (どちらも 01-03 の途中)。
        // warmup_bars=5 なので、01-03 0:00 より前の足は 0 本しかなく、5 本そろうのは
        // 01-04 0:00 が最初(series.rs::earliest_from と同じ規則)。
        let start = utc_secs(2024, 1, 3, 22, 0);
        let bars: Vec<Bar> = (0..10).map(|i| bar_at(start + i * M5)).collect();

        let (from, _to) = resolve_period(
            &bars,
            None,
            Some(NaiveDate::from_ymd_opt(2024, 1, 10).unwrap()),
            5,
        )
        .expect("enough total bars to satisfy warmup");
        assert_eq!(from, utc_secs(2024, 1, 4, 0, 0));
    }

    #[test]
    fn from_omitted_resolves_to_the_same_instant_when_the_warmup_boundary_bar_is_exactly_midnight()
    {
        let start = utc_secs(2024, 1, 3, 23, 35);
        let bars: Vec<Bar> = (0..10).map(|i| bar_at(start + i * M5)).collect();
        assert_eq!(bars[5].open_time, utc_secs(2024, 1, 4, 0, 0));

        let (from, _to) = resolve_period(
            &bars,
            None,
            Some(NaiveDate::from_ymd_opt(2024, 1, 10).unwrap()),
            5,
        )
        .unwrap();
        assert_eq!(from, utc_secs(2024, 1, 4, 0, 0));
    }

    #[test]
    fn to_omitted_resolves_to_the_day_after_the_last_bars_utc_date() {
        let start = utc_secs(2024, 1, 1, 0, 0);
        let bars: Vec<Bar> = (0..10).map(|i| bar_at(start + i * M5)).collect(); // all within Jan 1

        let (_from, to) = resolve_period(
            &bars,
            Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            None,
            0,
        )
        .unwrap();
        assert_eq!(to, utc_secs(2024, 1, 2, 0, 0));
    }

    #[test]
    fn from_at_or_after_to_is_rejected() {
        let bars: Vec<Bar> = (0..5).map(|i| bar_at(i * M5)).collect();
        let d = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();

        let err = resolve_period(&bars, Some(d), Some(d), 0).unwrap_err();
        assert!(matches!(err, SimError::Args(_)), "from == to");

        let after = NaiveDate::from_ymd_opt(2024, 1, 3).unwrap();
        let err = resolve_period(&bars, Some(after), Some(d), 0).unwrap_err();
        assert!(matches!(err, SimError::Args(_)), "from > to");
    }

    #[test]
    fn empty_bars_is_rejected_regardless_of_from_to() {
        let err = resolve_period(&[], None, None, 0).unwrap_err();
        assert!(matches!(err, SimError::Args(_)));

        let d = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
        let err = resolve_period(&[], Some(d), Some(d.succ_opt().unwrap()), 0).unwrap_err();
        assert!(
            matches!(err, SimError::Args(_)),
            "even with explicit from/to, zero loaded bars must be rejected"
        );
    }

    #[test]
    fn from_omitted_without_enough_warmup_bars_is_rejected() {
        let bars: Vec<Bar> = (0..3).map(|i| bar_at(i * M5)).collect(); // only 3 bars total
        let err = resolve_period(
            &bars,
            None,
            Some(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
            10,
        )
        .unwrap_err();
        assert!(matches!(err, SimError::Args(_)));
    }

    #[test]
    fn naive_date_to_utc_secs_is_midnight() {
        let d = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
        assert_eq!(naive_date_to_utc_secs(d), utc_secs(2024, 3, 10, 0, 0));
    }

    #[test]
    fn print_columns_and_kv_table_do_not_panic_on_empty_input() {
        // 出力ヘルパーはログではなく stdout に書くため assert できないが、空入力・幅 0 の
        // 境界で panic しないことだけは確認する(`max()` の `unwrap_or` 等)。
        let mut buf: Vec<u8> = Vec::new();
        print_kv_table(&mut buf, &[]).unwrap();
        print_columns(&mut buf, &["a", "b"], &[]).unwrap();
    }

    /// 常に書き込みに失敗する出力先(パイプが閉じた状況の再現)。
    struct BrokenPipe;
    impl std::io::Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn output_write_failure_is_returned_as_an_error_not_a_panic() {
        let err = print_kv_table(&mut BrokenPipe, &[("a".to_string(), "b".to_string())])
            .expect_err("a broken pipe must surface as Err");
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    // ---- 書き込みには成功するが flush が失敗する出力先(ディスクフル等)でも、各 print_* の
    // 末尾の `out.flush()` の失敗が `Err` として呼び出し元(`output_failed` 経路)に伝わる
    // ことを確認する(`out` の Drop 時の暗黙の flush はエラーを握りつぶすため) ----

    /// 書き込みは必ず成功するが、flush は必ず失敗する出力先。
    struct FlushFails;
    impl std::io::Write for FlushFails {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::Other))
        }
    }

    #[test]
    fn print_backfill_surfaces_a_flush_failure() {
        let report = fetch::BackfillReport {
            days: 1,
            saved: 1,
            one_sided: 0,
            invalid: 0,
            failed: Vec::new(),
        };
        for json in [false, true] {
            let err = print_backfill(&mut FlushFails, json, &report)
                .expect_err(&format!("flush failure must surface as Err (json={json})"));
            assert_eq!(err.kind(), std::io::ErrorKind::Other);
        }
    }

    #[test]
    fn print_benchmark_surfaces_a_flush_failure() {
        let benchmarks = vec![sample_benchmark(20, 1, 100, 90)];
        for json in [false, true] {
            let err = print_benchmark(&mut FlushFails, &benchmarks, json)
                .expect_err(&format!("flush failure must surface as Err (json={json})"));
            assert_eq!(err.kind(), std::io::ErrorKind::Other);
        }
    }

    #[test]
    fn print_run_surfaces_a_flush_failure() {
        let record = RunRecord {
            params: crate::script::ParamSet::new(),
            status: RunStatus::Ok,
            metrics: Some(sample_metrics()),
        };
        for json in [false, true] {
            let err = print_run(
                &mut FlushFails,
                &record,
                1.0,
                Uuid::nil(),
                Uuid::nil(),
                json,
            )
            .expect_err(&format!("flush failure must surface as Err (json={json})"));
            assert_eq!(err.kind(), std::io::ErrorKind::Other);
        }
    }

    #[test]
    fn print_sweep_surfaces_a_flush_failure() {
        let top: Vec<store::RunSummary> = Vec::new();
        for json in [false, true] {
            let err = print_sweep(
                &mut FlushFails,
                &top,
                1.0,
                Uuid::nil(),
                Uuid::nil(),
                0,
                json,
            )
            .expect_err(&format!("flush failure must surface as Err (json={json})"));
            assert_eq!(err.kind(), std::io::ErrorKind::Other);
        }
    }

    // ---- --json 出力の内容(`print_*` は stdout に直接書くため、実プロセスの標準出力を
    // 奪わずに内容を検証できるよう、JSON の組み立てだけを行う `*_json` 関数を単体テストする。
    // cli_test.rs(結合テスト)側は、この内容ではなく「終了コードと DB の状態」を確認する) ----

    fn dummy_leg() -> crate::benchmark::Leg {
        crate::benchmark::Leg {
            a: 0,
            b: 1,
            direction: 1,
            ideal_milli: 0,
            realizable_milli: 0,
        }
    }

    fn sample_benchmark(
        theta_pips: i64,
        leg_count: usize,
        ideal_milli: i64,
        realizable_milli: i64,
    ) -> Benchmark {
        Benchmark {
            theta_pips,
            legs: (0..leg_count).map(|_| dummy_leg()).collect(),
            ideal_milli,
            realizable_milli,
            labels: Vec::new(),
        }
    }

    #[test]
    fn benchmark_json_includes_leg_count_ideal_and_realizable_pips_for_each_theta() {
        let cases = [(20i64, 3usize, 900i64, 850i64), (50, 1, 200, 150)];
        let benchmarks: Vec<Benchmark> = cases
            .iter()
            .map(|&(theta, legs, ideal, realizable)| {
                sample_benchmark(theta, legs, ideal, realizable)
            })
            .collect();

        let value = benchmark_json(&benchmarks);
        let by_theta = value
            .get("by_theta")
            .expect("benchmark_json must have a top-level by_theta key");
        for (theta, leg_count, ideal_milli, realizable_milli) in cases {
            let entry = by_theta
                .get(theta.to_string())
                .unwrap_or_else(|| panic!("by_theta must have an entry for theta {theta}"));
            assert_eq!(entry["leg_count"], serde_json::json!(leg_count));
            assert_eq!(
                entry["ideal_pips"],
                serde_json::json!(milli_to_pips(ideal_milli))
            );
            assert_eq!(
                entry["realizable_pips"],
                serde_json::json!(milli_to_pips(realizable_milli))
            );
        }
    }

    fn sample_metrics() -> crate::eval::Metrics {
        crate::eval::Metrics {
            total_pips: 12.5,
            trade_count: 3,
            win_rate: 0.6667,
            max_drawdown_pips: 4.0,
            time_in_market: 0.5,
            protective_stop_count: 1,
            segments: [0.0; 6],
            by_theta: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn run_json_ok_status_includes_metrics_and_omits_error() {
        let record = RunRecord {
            params: crate::script::ParamSet::new(),
            status: RunStatus::Ok,
            metrics: Some(sample_metrics()),
        };
        let value = run_json(&record, 1.23, Uuid::nil(), Uuid::nil());
        assert_eq!(value["status"], "ok");
        assert_eq!(value["elapsed_secs"], 1.23);
        assert!(
            value.get("error").is_none(),
            "ok status must not have an error key"
        );
        assert_eq!(value["metrics"]["total_pips"], 12.5);
        assert_eq!(value["metrics"]["trade_count"], 3);
    }

    #[test]
    fn run_json_script_error_status_includes_error_and_omits_metrics() {
        let record = RunRecord {
            params: crate::script::ParamSet::new(),
            status: RunStatus::ScriptError {
                open_time: 0,
                message: "boom".to_string(),
            },
            metrics: None,
        };
        let value = run_json(&record, 0.5, Uuid::nil(), Uuid::nil());
        assert_eq!(value["status"], "script_error");
        assert!(
            value["error"].as_str().unwrap().contains("boom"),
            "error message must be present: {value}"
        );
        assert!(
            value.get("metrics").is_none(),
            "script_error status must not have a metrics key"
        );
    }

    #[test]
    fn sweep_json_includes_elapsed_total_runs_and_top_run_fields() {
        let top = vec![store::RunSummary {
            params: serde_json::json!({"entry": 20}),
            total_pips: 42.0,
            trade_count: 5,
            metrics: serde_json::json!({"segments": vec![0.0; 6]}),
        }];
        let value = sweep_json(&top, 3.21, Uuid::nil(), Uuid::nil(), 7);
        assert_eq!(value["elapsed_secs"], 3.21);
        assert_eq!(value["total_runs"], 7);
        let top_runs = value["top_runs"]
            .as_array()
            .expect("top_runs must be an array");
        assert_eq!(top_runs.len(), 1);
        assert_eq!(top_runs[0]["total_pips"], 42.0);
        assert_eq!(top_runs[0]["trade_count"], 5);
        assert_eq!(top_runs[0]["params"]["entry"], 20);
    }

    #[test]
    fn backfill_json_always_includes_every_count_and_the_failed_list() {
        let full = backfill_json(&fetch::BackfillReport {
            days: 2,
            saved: 100,
            one_sided: 1,
            invalid: 0,
            failed: Vec::new(),
        });
        assert_eq!(full["days"], 2);
        assert_eq!(full["saved"], 100);
        assert_eq!(full["one_sided"], 1);
        assert_eq!(full["invalid"], 0);
        assert_eq!(full["failed"], serde_json::json!([] as [String; 0]));

        let partial = backfill_json(&fetch::BackfillReport {
            days: 3,
            saved: 10,
            one_sided: 0,
            invalid: 2,
            failed: vec!["20240101 ASK".to_string()],
        });
        assert_eq!(partial["days"], 3);
        assert_eq!(partial["saved"], 10);
        assert_eq!(partial["invalid"], 2);
        assert_eq!(partial["failed"], serde_json::json!(["20240101 ASK"]));
    }

    /// ログをプロセスの実際の標準エラーではなく、テストが検証できるバッファに溜める
    /// `tracing_subscriber::fmt::MakeWriter`。`cli_test.rs` の `CapturingWriter` と同じ構成
    /// (private なのでここで複製する。`series.rs` のテストヘルパーと同じ理由)。
    #[derive(Clone, Default)]
    struct CapturingWriter {
        buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl CapturingWriter {
        fn contents(&self) -> String {
            String::from_utf8(self.buf.lock().expect("lock poisoned").clone())
                .expect("log output must be UTF-8")
        }
    }

    impl std::io::Write for CapturingWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.buf
                .lock()
                .expect("lock poisoned")
                .extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingWriter {
        type Writer = CapturingWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn format_utc_rfc3339_logs_a_warning_when_falling_back_to_epoch_seconds() {
        // `chrono::DateTime::<Utc>::from_timestamp` が必ず `None` を返す値(chrono が表現
        // できる年範囲を大きく外れる)。
        let epoch_secs = i64::MIN;

        let writer = CapturingWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer.clone())
            .with_ansi(false)
            .finish();
        let formatted = {
            let _guard = tracing::subscriber::set_default(subscriber);
            format_utc_rfc3339(epoch_secs)
        };

        assert_eq!(
            formatted,
            epoch_secs.to_string(),
            "the fallback value itself must stay the epoch-seconds string"
        );
        let log = writer.contents();
        assert!(
            log.contains("WARN"),
            "falling back to the epoch-seconds representation must log a WARN: {log}"
        );
        assert!(
            log.contains(&epoch_secs.to_string()),
            "the WARN log must include the epoch seconds value that failed to convert: {log}"
        );
    }

    #[test]
    fn effective_jobs_keeps_the_requested_value_when_it_is_the_smallest() {
        assert_eq!(effective_jobs(2, 10, 8), 2);
    }

    #[test]
    fn effective_jobs_clamps_to_available_parallelism_when_it_is_the_smallest() {
        assert_eq!(effective_jobs(16, 10, 8), 8);
    }

    #[test]
    fn effective_jobs_clamps_to_the_run_count_when_it_is_the_smallest() {
        assert_eq!(effective_jobs(16, 3, 8), 3);
    }

    #[test]
    fn effective_jobs_returns_one_when_requested_is_zero() {
        assert_eq!(effective_jobs(0, 5, 8), 1);
    }

    #[test]
    fn effective_jobs_returns_one_when_the_run_count_is_zero() {
        assert_eq!(effective_jobs(4, 0, 8), 1);
    }

    #[test]
    fn effective_jobs_returns_one_when_available_parallelism_is_zero() {
        assert_eq!(effective_jobs(4, 10, 0), 1);
    }
}
