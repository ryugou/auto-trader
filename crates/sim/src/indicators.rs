//! 指標系列（`f64`、円）の逐次計算と、計算結果のキャッシュ（spec 6.3 章）。
//!
//! 各系列の要素 `i` は、`crates/market/src/indicators.rs`（`Decimal` 版、正本）の
//! 対応する関数を系列の `[0..=i]` に適用した値と一致する（差は `1e-6` 以下）。
//! ここでは同じ値を O(n) の 1 回の走査で求めるため、`market` クレートの各関数の
//! 初期値の作り方・平滑化方法をそのまま f64 の逐次計算に書き直してある
//! （`market` クレートはテストでのみ参照し、依存は増やさない）。
//!
//! `period` や `mult` の範囲検証（1〜1000、10〜1000）は、スクリプト実行時の
//! 引数チェック（Task 5 `script.rs` の `ctx` 層）の責務であり、ここでは行わない。
//! 本数が足りない添字は `f64::NAN` を返すことだけを保証する（計画 Task 3 の記述）。

use crate::series::{Tf, TfSeries};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// 8 種の指標（spec 6.3 章の表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndicatorKind {
    Sma,
    Ema,
    Rsi,
    Atr,
    Adx,
    Bb,
    Donchian,
    Keltner,
}

/// 指標系列のキャッシュキー。`mult` を使わない指標（`sma`/`ema`/`rsi`/`atr`/`adx`/`donchian`）は
/// `mult_x100 = 0` とする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndicatorKey {
    pub tf: Tf,
    pub kind: IndicatorKind,
    pub period: u32,
    pub mult_x100: u32,
}

/// 指標系列の値。1 本の線（`Single`）、`lower/middle/upper` の帯（`Band`、bb・keltner）、
/// `lower/upper` のチャネル（`Channel`、donchian）のいずれか。
#[derive(Debug, Clone)]
pub enum IndicatorSeries {
    Single(Vec<f64>),
    Band {
        lower: Vec<f64>,
        middle: Vec<f64>,
        upper: Vec<f64>,
    },
    Channel {
        lower: Vec<f64>,
        upper: Vec<f64>,
    },
}

impl IndicatorSeries {
    /// `IndicatorCache` の LRU 判定に使う概算サイズ（バイト）。`Vec<f64>` の要素数 × 8 の
    /// 合計とする（`Vec` の容量と要素のオーバーヘッドは無視する: 破棄の判断材料として十分な
    /// 精度があればよく、厳密なヒープ使用量は不要）。
    fn approx_bytes(&self) -> usize {
        const F64_BYTES: usize = std::mem::size_of::<f64>();
        match self {
            IndicatorSeries::Single(v) => v.len() * F64_BYTES,
            IndicatorSeries::Band {
                lower,
                middle,
                upper,
            } => (lower.len() + middle.len() + upper.len()) * F64_BYTES,
            IndicatorSeries::Channel { lower, upper } => (lower.len() + upper.len()) * F64_BYTES,
        }
    }
}

/// `series` の `[0..=i]` に対応する指標値を、すべての添字 `i` について計算する。
/// 本数が足りない添字は `f64::NAN`（Task 3 の保証範囲。`period`/`mult` の範囲検証はしない）。
pub fn compute(series: &TfSeries, key: IndicatorKey) -> IndicatorSeries {
    let period = key.period as usize;
    let mult = key.mult_x100 as f64 / 100.0;
    match key.kind {
        IndicatorKind::Sma => IndicatorSeries::Single(sma_series(&series.close, period)),
        IndicatorKind::Ema => IndicatorSeries::Single(ema_series(&series.close, period)),
        IndicatorKind::Rsi => IndicatorSeries::Single(rsi_series(&series.close, period)),
        IndicatorKind::Atr => {
            IndicatorSeries::Single(atr_series(&series.high, &series.low, &series.close, period))
        }
        IndicatorKind::Adx => {
            IndicatorSeries::Single(adx_series(&series.high, &series.low, &series.close, period))
        }
        IndicatorKind::Bb => {
            let (lower, middle, upper) = bb_series(&series.close, period, mult);
            IndicatorSeries::Band {
                lower,
                middle,
                upper,
            }
        }
        IndicatorKind::Donchian => {
            let (lower, upper) = donchian_series(&series.high, &series.low, period);
            IndicatorSeries::Channel { lower, upper }
        }
        IndicatorKind::Keltner => {
            let (lower, middle, upper) =
                keltner_series(&series.high, &series.low, &series.close, period, mult);
            IndicatorSeries::Band {
                lower,
                middle,
                upper,
            }
        }
    }
}

/// `market::sma(closes, period)` を `[0..=i]` について走査する逐次版。
/// 添字 `i` は `i + 1 >= period` のとき定義される（末尾 `period` 本の単純平均）。
fn sma_series(closes: &[f64], period: usize) -> Vec<f64> {
    let n = closes.len();
    let mut result = vec![f64::NAN; n];
    if period == 0 {
        return result;
    }
    let mut sum = 0.0;
    for (i, &c) in closes.iter().enumerate() {
        sum += c;
        if i >= period {
            sum -= closes[i - period];
        }
        if i + 1 >= period {
            result[i] = sum / period as f64;
        }
    }
    result
}

/// `market::ema(closes, period)` の逐次版。種は「先頭 `period` 本」の単純平均（`market` の
/// `sma(&prices[..period], period)` が末尾ではなく先頭を使うことに注意）。この種は添字が
/// 進んでも変わらないため、添字 `period - 1` で 1 度だけ計算し、以降は標準の平滑化を 1 本ずつ
/// 適用するだけで、`market` を `[0..=i]` に再適用した結果と一致する。
fn ema_series(closes: &[f64], period: usize) -> Vec<f64> {
    let n = closes.len();
    let mut result = vec![f64::NAN; n];
    if period == 0 || n < period {
        return result;
    }
    let multiplier = 2.0 / (period as f64 + 1.0);
    let seed: f64 = closes[..period].iter().sum::<f64>() / period as f64;
    let mut ema_val = seed;
    result[period - 1] = ema_val;
    for (i, &c) in closes.iter().enumerate().skip(period) {
        ema_val = (c - ema_val) * multiplier + ema_val;
        result[i] = ema_val;
    }
    result
}

/// `market::rsi(closes, period)` の逐次版。`market` はウィルダー平滑化を行わず、直前の
/// `period` 本の値動きだけから毎回平均を取り直す（スライディングウィンドウ）ため、
/// 窓の合計を増減するだけで `[0..=i]` の再適用と一致する。
///
/// f64 の加減算では、窓から値が抜けた後に微小な残差が合計に残り得る。すると
/// 「窓に下落が 1 つもない」場合でも `avg_loss == 0.0` が偽になり、`market`（窓ごとに Decimal で
/// 集計し直して `Decimal::ZERO` と比較する）と分岐がずれる。これを防ぐため、窓の中の上昇
/// （`d > 0`）と下落（`d < 0`）の本数を整数で数え、本数が 0 になったら対応する合計を
/// 厳密に `0.0` へ戻す。`d == 0` は `market` と同じく下落側に 0 を足すだけで、本数には数えない。
fn rsi_series(closes: &[f64], period: usize) -> Vec<f64> {
    let n = closes.len();
    let mut result = vec![f64::NAN; n];
    if period == 0 || n < period + 1 {
        return result;
    }
    // diffs[k] = closes[k+1] - closes[k] (k = 0..n-2)。
    let diffs: Vec<f64> = closes.windows(2).map(|w| w[1] - w[0]).collect();
    let mut gain_sum = 0.0;
    let mut loss_sum = 0.0;
    let mut gain_count = 0usize;
    let mut loss_count = 0usize;
    for (k, &d) in diffs.iter().enumerate() {
        if d > 0.0 {
            gain_sum += d;
            gain_count += 1;
        } else {
            loss_sum += -d;
            if d < 0.0 {
                loss_count += 1;
            }
        }
        if k >= period {
            let old = diffs[k - period];
            if old > 0.0 {
                gain_sum -= old;
                gain_count -= 1;
            } else {
                loss_sum -= -old;
                if old < 0.0 {
                    loss_count -= 1;
                }
            }
        }
        if gain_count == 0 {
            gain_sum = 0.0;
        }
        if loss_count == 0 {
            loss_sum = 0.0;
        }
        // diffs[k] は closes[k+1] に対応するので、bar 添字は k+1。
        let i = k + 1;
        if i >= period {
            let avg_gain = gain_sum / period as f64;
            let avg_loss = loss_sum / period as f64;
            result[i] = if avg_loss == 0.0 {
                100.0
            } else {
                let rs = avg_gain / avg_loss;
                100.0 - 100.0 / (1.0 + rs)
            };
        }
    }
    result
}

/// bar `k` (k >= 1) の True Range。
fn true_range(highs: &[f64], lows: &[f64], closes: &[f64], k: usize) -> f64 {
    let hl = highs[k] - lows[k];
    let hc = (highs[k] - closes[k - 1]).abs();
    let lc = (lows[k] - closes[k - 1]).abs();
    hl.max(hc).max(lc)
}

/// `market::atr(highs, lows, closes, period)` の逐次版。最初の ATR は先頭 `period` 本の TR の
/// 単純平均（添字 `period` 固定、添字が進んでも変わらない）、以降はウィルダー平滑化。
fn atr_series(highs: &[f64], lows: &[f64], closes: &[f64], period: usize) -> Vec<f64> {
    let n = closes.len();
    let mut result = vec![f64::NAN; n];
    if period == 0 || n < period + 1 {
        return result;
    }
    let seed: f64 = (1..=period)
        .map(|k| true_range(highs, lows, closes, k))
        .sum::<f64>()
        / period as f64;
    let mut atr_val = seed;
    result[period] = atr_val;
    let p = period as f64;
    for (i, slot) in result.iter_mut().enumerate().skip(period + 1) {
        let tr = true_range(highs, lows, closes, i);
        atr_val = (atr_val * (p - 1.0) + tr) / p;
        *slot = atr_val;
    }
    result
}

/// `market::adx(highs, lows, closes, period)` の逐次版。`+DM`/`-DM`/`TR` のウィルダー平滑化と、
/// `DX` 列の先頭 `period` 本の単純平均を種にした ADX のウィルダー平滑化を、bar が 1 本増える
/// たびに 1 ステップだけ進める。`market` が `smooth_tr == 0` の回を `DX` 列に積まない（`continue`）
/// 挙動もそのまま再現する。
fn adx_series(highs: &[f64], lows: &[f64], closes: &[f64], period: usize) -> Vec<f64> {
    let n = closes.len();
    let mut result = vec![f64::NAN; n];
    if period == 0 || n < period * 2 + 1 {
        return result;
    }
    let p = period as f64;
    let m = n - 1; // tr/dm の本数。tr[j] は bar (j, j+1) の対。
    let mut plus_dm = vec![0.0f64; m];
    let mut minus_dm = vec![0.0f64; m];
    let mut tr = vec![0.0f64; m];
    for j in 0..m {
        let k = j + 1;
        let high_diff = highs[k] - highs[k - 1];
        let low_diff = lows[k - 1] - lows[k];
        plus_dm[j] = if high_diff > low_diff && high_diff > 0.0 {
            high_diff
        } else {
            0.0
        };
        minus_dm[j] = if low_diff > high_diff && low_diff > 0.0 {
            low_diff
        } else {
            0.0
        };
        tr[j] = true_range(highs, lows, closes, k);
    }

    let mut smooth_plus: f64 = plus_dm[..period].iter().sum();
    let mut smooth_minus: f64 = minus_dm[..period].iter().sum();
    let mut smooth_tr: f64 = tr[..period].iter().sum();

    let mut dx_vals: Vec<f64> = Vec::new();
    let mut adx_val: Option<f64> = None;
    let mut consumed = 0usize;

    for j in period..m {
        smooth_plus = smooth_plus - smooth_plus / p + plus_dm[j];
        smooth_minus = smooth_minus - smooth_minus / p + minus_dm[j];
        smooth_tr = smooth_tr - smooth_tr / p + tr[j];
        let bar_index = j + 1;

        if smooth_tr != 0.0 {
            let plus_di = smooth_plus / smooth_tr * 100.0;
            let minus_di = smooth_minus / smooth_tr * 100.0;
            let di_sum = plus_di + minus_di;
            let dx = if di_sum == 0.0 {
                0.0
            } else {
                (plus_di - minus_di).abs() / di_sum * 100.0
            };
            dx_vals.push(dx);
        }

        if adx_val.is_none() {
            if dx_vals.len() >= period {
                let seed: f64 = dx_vals[..period].iter().sum::<f64>() / p;
                adx_val = Some(seed);
                consumed = period;
            }
        } else {
            while dx_vals.len() > consumed {
                let next = dx_vals[consumed];
                let prev = adx_val.expect("adx_val is Some in this branch");
                adx_val = Some((prev * (p - 1.0) + next) / p);
                consumed += 1;
            }
        }

        result[bar_index] = adx_val.unwrap_or(f64::NAN);
    }
    result
}

/// `market::bollinger_bands(closes, period, mult)` の逐次版。`market` と同じ計算順で、
/// 窓の平均 `middle` を求めたあと、窓の各値について `(x - middle)^2` の和を `period` で割って
/// 母分散を窓ごとに直接計算する（O(n × period)）。
///
/// 平方和の増減（`sum_sq / p - mean^2`）方式は、価格 150 前後では平方が約 22500 になり桁落ちし、
/// さらに増減の丸め誤差が系列長に応じて蓄積して spec 6.3 章の許容差 1e-6 を超えるため採らない。
/// 計算量は period が最大 1000 でも、結果がキャッシュされるので許容する。
/// 偏差の 2 乗和は負にならないので、`sqrt` が `NaN` を返すことはない。
fn bb_series(closes: &[f64], period: usize, mult: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = closes.len();
    let mut lower = vec![f64::NAN; n];
    let mut middle = vec![f64::NAN; n];
    let mut upper = vec![f64::NAN; n];
    if period == 0 {
        return (lower, middle, upper);
    }
    let p = period as f64;
    let mut sum = 0.0;
    for (i, &c) in closes.iter().enumerate() {
        sum += c;
        if i >= period {
            sum -= closes[i - period];
        }
        if i + 1 >= period {
            let mean = sum / p;
            let window = &closes[i + 1 - period..=i];
            let variance = window
                .iter()
                .map(|&x| {
                    let diff = x - mean;
                    diff * diff
                })
                .sum::<f64>()
                / p;
            let band = variance.sqrt() * mult;
            lower[i] = mean - band;
            middle[i] = mean;
            upper[i] = mean + band;
        }
    }
    (lower, middle, upper)
}

/// `market::donchian_channel(highs, lows, period, include_current = true)` の逐次版。
/// 窓内の最大 high・最小 low を単調デック（monotonic deque）で O(n) に保つ。
fn donchian_series(highs: &[f64], lows: &[f64], period: usize) -> (Vec<f64>, Vec<f64>) {
    let n = highs.len();
    let mut lower = vec![f64::NAN; n];
    let mut upper = vec![f64::NAN; n];
    if period == 0 {
        return (lower, upper);
    }
    let mut max_dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut min_dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for i in 0..n {
        while let Some(&back) = max_dq.back() {
            if highs[back] <= highs[i] {
                max_dq.pop_back();
            } else {
                break;
            }
        }
        max_dq.push_back(i);
        while let Some(&back) = min_dq.back() {
            if lows[back] >= lows[i] {
                min_dq.pop_back();
            } else {
                break;
            }
        }
        min_dq.push_back(i);

        while let Some(&front) = max_dq.front() {
            if front + period <= i {
                max_dq.pop_front();
            } else {
                break;
            }
        }
        while let Some(&front) = min_dq.front() {
            if front + period <= i {
                min_dq.pop_front();
            } else {
                break;
            }
        }

        if i + 1 >= period {
            upper[i] = highs[*max_dq
                .front()
                .expect("window is non-empty once i+1>=period")];
            lower[i] = lows[*min_dq
                .front()
                .expect("window is non-empty once i+1>=period")];
        }
    }
    (lower, upper)
}

/// `market::keltner_channels(highs, lows, closes, period, atr_mult)` の逐次版。`middle` は
/// `ema_series`、帯幅は `atr_series × mult`。`market` は `ema` と `atr` のどちらかが `None` なら
/// 全体を `None` にする（`?` 演算子）ため、ここも両方が定義されている添字だけ値を持つ。
fn keltner_series(
    highs: &[f64],
    lows: &[f64],
    closes: &[f64],
    period: usize,
    mult: f64,
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = closes.len();
    let ema = ema_series(closes, period);
    let atr = atr_series(highs, lows, closes, period);
    let mut lower = vec![f64::NAN; n];
    let mut middle = vec![f64::NAN; n];
    let mut upper = vec![f64::NAN; n];
    for i in 0..n {
        if !ema[i].is_nan() && !atr[i].is_nan() {
            let band = atr[i] * mult;
            lower[i] = ema[i] - band;
            middle[i] = ema[i];
            upper[i] = ema[i] + band;
        }
    }
    (lower, middle, upper)
}

/// `(時間足, 指標, period, 丸めた mult)` ごとに系列全体を 1 度だけ計算し、複数スレッドから
/// `Arc` で共有するキャッシュ（spec 6.3 章「キャッシュ」）。
///
/// ロックの粒度: 全体の `Mutex` が守るのは `HashMap` の読み書きと LRU の記帳だけで、
/// `compute_fn`（BB は O(n × period)、大きな系列では 0.3 秒程度）は `Mutex` の外で呼ぶ。
/// キーごとに `OnceLock` の「計算枠」を持ち、
/// - 同じキーの同時要求は `OnceLock::get_or_init` が 1 回だけ計算し、他は完了を待つ。
/// - 異なるキーの計算と、キャッシュ済みキーの取得（ヒット）は互いを待たず並行して進む。
///
/// `compute_fn` が panic しても全体の `Mutex` は poison しない（`Mutex` の外で呼ぶため）。
/// 枠は未初期化のまま残り、次の要求が計算をやり直す。
///
/// 破棄は速度にだけ影響し、結果には影響しない（spec 6.3）。不変条件:
/// `used_bytes` は全エントリの `bytes` の合計と一致する。
#[derive(Debug)]
pub(crate) struct IndicatorCache {
    budget_bytes: usize,
    inner: Mutex<CacheInner>,
}

/// 1 キーの計算枠。未計算の間は `get()` が `None`。
type Slot = Arc<OnceLock<Arc<IndicatorSeries>>>;

#[derive(Debug)]
struct CacheEntry {
    slot: Slot,
    /// サイズ登録済みなら `approx_bytes()`、未登録（計算中・計算直後）は 0。
    bytes: usize,
    last_used: u64,
}

#[derive(Debug, Default)]
struct CacheInner {
    map: HashMap<IndicatorKey, CacheEntry>,
    used_bytes: usize,
    clock: u64,
}

const POISONED_MESSAGE: &str = "indicator cache mutex poisoned: another thread panicked while \
     updating the cache bookkeeping (not while computing an indicator); the process state is \
     inconsistent, restart the run";

impl IndicatorCache {
    pub(crate) fn new(budget_mb: usize) -> Self {
        IndicatorCache {
            budget_bytes: budget_mb.saturating_mul(1024 * 1024),
            inner: Mutex::new(CacheInner::default()),
        }
    }

    /// `key` の系列を返す。キャッシュ済みならそれを、未済なら `compute_fn()` の結果を
    /// キャッシュしてから返す。容量超過時は最後に使われてから最も時間がたったキーから破棄する
    /// （計算中のキーと、いま要求中のキーは破棄しない）。
    ///
    /// 1 本の系列だけで予算を超える場合は、計算結果を返したうえでキャッシュには残さない
    /// （`used_bytes` が予算を超えたままになるのを防ぐ）。次に同じキーを要求すると再計算
    /// になるが、影響は速度だけで結果は変わらない。
    pub(crate) fn get_or_compute(
        &self,
        key: IndicatorKey,
        compute_fn: impl FnOnce() -> IndicatorSeries,
    ) -> Arc<IndicatorSeries> {
        // 1. 記帳だけを `Mutex` の中で行い、計算枠を取り出す。
        let slot = {
            let mut guard = self.inner.lock().expect(POISONED_MESSAGE);
            guard.clock += 1;
            let now = guard.clock;
            let entry = guard.map.entry(key).or_insert_with(|| CacheEntry {
                slot: Arc::new(OnceLock::new()),
                bytes: 0,
                last_used: now,
            });
            entry.last_used = now;
            if let Some(series) = entry.slot.get() {
                return series.clone();
            }
            entry.slot.clone()
        };

        // 2. `Mutex` の外で計算する。同じキーの同時要求は `OnceLock` が 1 回にまとめる。
        let series = slot.get_or_init(|| Arc::new(compute_fn())).clone();

        // 3. サイズ未登録なら登録し、予算超過なら破棄する。
        let mut guard = self.inner.lock().expect(POISONED_MESSAGE);
        let registered_bytes = match guard.map.get_mut(&key) {
            Some(entry) if Arc::ptr_eq(&entry.slot, &slot) && entry.bytes == 0 => {
                entry.bytes = series.approx_bytes();
                entry.bytes
            }
            // 登録済み（他スレッドが先に登録した）か、破棄・差し替え済み。何もしない。
            _ => 0,
        };
        guard.used_bytes += registered_bytes;
        if registered_bytes > 0 {
            self.evict_over_budget(&mut guard, key);
            // 1 本だけで予算を超える系列は、自分以外を全部破棄しても予算に収まらない。
            // 予算を守るためキャッシュには残さず、呼び出し元にだけ返す（次の要求は再計算）。
            if guard.used_bytes > self.budget_bytes && registered_bytes > self.budget_bytes {
                let is_ours = guard
                    .map
                    .get(&key)
                    .is_some_and(|entry| Arc::ptr_eq(&entry.slot, &slot));
                // let chains は Rust 1.88 以降。MSRV 1.85 のため match で書く。
                let removed = if is_ours {
                    guard.map.remove(&key)
                } else {
                    None
                };
                if let Some(removed) = removed {
                    guard.used_bytes -= removed.bytes;
                }
            }
        }
        series
    }

    /// 予算を超えている間、最後に使われてから最も時間がたったエントリを破棄する。
    /// 計算が終わっていない枠、サイズ未登録（0 バイト）のエントリ、`protected` は対象外。
    /// 候補がなくなれば抜けるので、必ず停止する。
    fn evict_over_budget(&self, inner: &mut CacheInner, protected: IndicatorKey) {
        while inner.used_bytes > self.budget_bytes {
            let victim = inner
                .map
                .iter()
                .filter(|(k, e)| **k != protected && e.bytes > 0 && e.slot.get().is_some())
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| *k);
            let Some(victim) = victim else {
                break;
            };
            if let Some(removed) = inner.map.remove(&victim) {
                inner.used_bytes -= removed.bytes;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::series::TfSeries;
    use auto_trader_market::indicators as market;
    use rust_decimal::Decimal;
    use rust_decimal::prelude::FromPrimitive;

    /// 決定的な 400 本の系列（計画 Task 3 Step 3 で指示された式）。sin を使い、周期性と
    /// 緩やかな上昇トレンドの両方を持たせることで、8 種の指標すべてが意味のある値を返す。
    fn deterministic_series(n: usize) -> TfSeries {
        let close: Vec<f64> = (0..n)
            .map(|i| 150.0 + 0.3 * (i as f64 * 0.07).sin() + 0.001 * i as f64)
            .collect();
        let high: Vec<f64> = close.iter().map(|c| c + 0.05).collect();
        let low: Vec<f64> = close.iter().map(|c| c - 0.05).collect();
        let open: Vec<f64> = (0..n)
            .map(|i| if i == 0 { close[0] } else { close[i - 1] })
            .collect();
        let end_time: Vec<i64> = (0..n).map(|i| (i as i64 + 1) * 900).collect();
        TfSeries {
            open,
            high,
            low,
            close,
            end_time,
        }
    }

    fn to_decimal(v: &[f64]) -> Vec<Decimal> {
        v.iter()
            .map(|&x| Decimal::from_f64(x).expect("finite test price"))
            .collect()
    }

    fn assert_close(actual: f64, expected: Option<Decimal>, label: &str, i: usize) {
        match expected {
            None => assert!(
                actual.is_nan(),
                "{label} at {i}: expected NaN (market returned None), got {actual}"
            ),
            Some(dec) => {
                let expected_f64 = rust_decimal::prelude::ToPrimitive::to_f64(&dec)
                    .expect("decimal result fits in f64");
                assert!(
                    (actual - expected_f64).abs() <= 1e-6,
                    "{label} at {i}: expected {expected_f64}, got {actual} (diff {})",
                    (actual - expected_f64).abs()
                );
            }
        }
    }

    const PERIODS: [usize; 3] = [2, 14, 50];

    /// 全 period × 全添字の組。計画が指定する境界の添字（period-1・period・2*period 等）も含む。
    fn check_indices(n: usize, periods: &[usize]) -> Vec<(usize, usize)> {
        periods
            .iter()
            .flat_map(|&period| (0..n).map(move |i| (period, i)))
            .collect()
    }

    /// mid2 の整数列（0.0005 円単位）を円の f64 にする。2 進で正確に表せない値（150.0015 等）になる。
    fn closes_from_mid2(mid2: &[i64]) -> Vec<f64> {
        mid2.iter().map(|&m| m as f64 / 2000.0).collect()
    }

    fn assert_rsi_matches_market_at_every_index(mid2: &[i64], label: &str) {
        let closes = closes_from_mid2(mid2);
        let closes_dec = to_decimal(&closes);
        for period in [2usize, 3, 14] {
            let values = rsi_series(&closes, period);
            for i in 0..closes.len() {
                let expected = market::rsi(&closes_dec[..=i], period);
                assert_close(
                    values[i],
                    expected,
                    &format!("rsi[{label}, period={period}]"),
                    i,
                );
            }
        }
    }

    /// `start` から `steps` を順に足した mid2 列。
    fn walk(start: i64, steps: &[(usize, i64)]) -> Vec<i64> {
        let mut out = vec![start];
        let mut cur = start;
        for &(count, step) in steps {
            for _ in 0..count {
                cur += step;
                out.push(cur);
            }
        }
        out
    }

    #[test]
    fn rsi_is_100_after_declines_followed_by_a_long_run_of_gains() {
        // 下落 12 本の後に 40 本の連続上昇。上昇だけの窓では market は avg_loss == 0 で 100 を返す。
        let mid2 = walk(300_003, &[(2, -70_001), (10, -1), (40, 3)]);
        assert_rsi_matches_market_at_every_index(&mid2, "decline-then-gains");
    }

    #[test]
    fn rsi_matches_market_across_fully_flat_stretches() {
        // 同じ値が 20 本続く区間（変化がすべて 0）を、上昇・下落の前後に含む。
        let mid2 = walk(
            300_003,
            &[
                (3, 70_001),
                (2, -50_001),
                (3, 1),
                (20, 0),
                (6, -5),
                (20, 0),
                (8, 7),
            ],
        );
        assert_rsi_matches_market_at_every_index(&mid2, "flat-stretches");
    }

    #[test]
    fn rsi_matches_market_on_blocks_of_flat_up_down_with_mixed_magnitude_steps() {
        // 30 本ごとに「横ばい・下落・上昇・往復」を切り替え、各ブロック内の刻みは大小（1〜3 と
        // 最大 100,000 の mid2）を混ぜる。窓合計に残差が残りやすい系列。
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut mid2: i64 = 300_003;
        let mut series = Vec::new();
        for block in 0..400 {
            for _ in 0..30 {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let r = ((state >> 33) as i64).abs();
                let magnitude = if r % 5 == 0 { r % 100_000 } else { r % 3 };
                mid2 += match block % 4 {
                    0 => 0,
                    1 => -magnitude,
                    2 => magnitude,
                    _ => (r % 1301) - 600,
                };
                series.push(mid2);
            }
        }
        assert_rsi_matches_market_at_every_index(&series, "mixed-magnitude-blocks");
    }

    #[test]
    fn rsi_matches_market_for_windows_with_no_gains_only_declines_and_flats() {
        let mid2 = walk(
            300_003,
            &[(10, 3), (3, -70_001), (5, 0), (2, -11), (15, 0), (10, 5)],
        );
        assert_rsi_matches_market_at_every_index(&mid2, "no-gain-windows");
    }

    #[test]
    fn sma_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Sma,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("sma must produce Single");
            };
            let expected = market::sma(&closes_dec[..=i], period);
            assert_close(values[i], expected, "sma", i);
        }
    }

    #[test]
    fn ema_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Ema,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("ema must produce Single");
            };
            let expected = market::ema(&closes_dec[..=i], period);
            assert_close(values[i], expected, "ema", i);
        }
    }

    #[test]
    fn rsi_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Rsi,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("rsi must produce Single");
            };
            let expected = market::rsi(&closes_dec[..=i], period);
            assert_close(values[i], expected, "rsi", i);
        }
    }

    #[test]
    fn atr_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Atr,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("atr must produce Single");
            };
            let expected =
                market::atr(&highs_dec[..=i], &lows_dec[..=i], &closes_dec[..=i], period);
            assert_close(values[i], expected, "atr", i);
        }
    }

    #[test]
    fn adx_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Adx,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("adx must produce Single");
            };
            let expected =
                market::adx(&highs_dec[..=i], &lows_dec[..=i], &closes_dec[..=i], period);
            assert_close(values[i], expected, "adx", i);
        }
    }

    #[test]
    fn adx_matches_market_when_a_fully_flat_stretch_precedes_the_movement() {
        // 先頭 80 本は high・low・close がすべて 150.0 で一定（TR・DM がすべて 0 なので、
        // `smooth_tr == 0` の回が続く）。その後に値動きが始まる。
        let flat_bars = 80;
        let n = 400;
        let moving = deterministic_series(n - flat_bars);
        let mut series = TfSeries {
            open: vec![150.0; flat_bars],
            high: vec![150.0; flat_bars],
            low: vec![150.0; flat_bars],
            close: vec![150.0; flat_bars],
            end_time: (0..flat_bars).map(|i| (i as i64 + 1) * 900).collect(),
        };
        series.open.extend(&moving.open);
        series.high.extend(&moving.high);
        series.low.extend(&moving.low);
        series.close.extend(&moving.close);
        series
            .end_time
            .extend((flat_bars..n).map(|i| (i as i64 + 1) * 900));
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Adx,
                period: period as u32,
                mult_x100: 0,
            };
            let IndicatorSeries::Single(values) = compute(&series, key) else {
                panic!("adx must produce Single");
            };
            let expected =
                market::adx(&highs_dec[..=i], &lows_dec[..=i], &closes_dec[..=i], period);
            assert_close(values[i], expected, "adx(flat prefix)", i);
        }
    }

    #[test]
    fn bb_matches_market_within_tolerance_and_keys_map_to_spec_names() {
        let n = 400;
        let series = deterministic_series(n);
        let closes_dec = to_decimal(&series.close);
        let mult_dec = Decimal::from_f64(2.0).unwrap();
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Bb,
                period: period as u32,
                mult_x100: 200,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Band {
                lower,
                middle,
                upper,
            } = result
            else {
                panic!("bb must produce Band");
            };
            let expected = market::bollinger_bands(&closes_dec[..=i], period, mult_dec);
            assert_close(lower[i], expected.map(|(l, _, _)| l), "bb.lower", i);
            assert_close(middle[i], expected.map(|(_, m, _)| m), "bb.middle", i);
            assert_close(upper[i], expected.map(|(_, _, u)| u), "bb.upper", i);
        }
    }

    /// 乱数を使わない決定的な乱歩（mid2 の整数、0.0005 円単位）。3 本に 1 本程度は刻みを 0 に
    /// して同じ値が続く区間を作る。平方和の増減方式は、長い系列でここに桁落ちと丸め誤差の
    /// 蓄積が現れる（400 本では検出できない）。
    fn random_walk_closes(n: usize) -> Vec<f64> {
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut mid2: i64 = 300_000; // 150.0 円
        let mut closes = Vec::with_capacity(n);
        for _ in 0..n {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let r = (state >> 33) as i64;
            let step = if r % 3 == 0 { 0 } else { (r / 3) % 7 - 3 };
            mid2 += step;
            closes.push(mid2 as f64 / 2000.0);
        }
        closes
    }

    #[test]
    fn bb_matches_market_on_long_random_walk_without_cancellation_error() {
        let n = 100_000;
        let tail = 2_000;
        let closes = random_walk_closes(n);
        let series = TfSeries {
            open: closes.clone(),
            high: closes.clone(),
            low: closes.clone(),
            close: closes.clone(),
            end_time: (0..n).map(|i| (i as i64 + 1) * 900).collect(),
        };
        let closes_dec = to_decimal(&closes);
        for period in [2usize, 14, 50] {
            for mult_x100 in [200u32, 1000] {
                let key = IndicatorKey {
                    tf: Tf::M15,
                    kind: IndicatorKind::Bb,
                    period: period as u32,
                    mult_x100,
                };
                let IndicatorSeries::Band {
                    lower,
                    middle,
                    upper,
                } = compute(&series, key)
                else {
                    panic!("bb must produce Band");
                };
                let mult_dec = Decimal::from(mult_x100) / Decimal::from(100);
                for i in (n - tail)..n {
                    let expected = market::bollinger_bands(&closes_dec[..=i], period, mult_dec);
                    let label = format!("bb(period={period}, mult_x100={mult_x100})");
                    assert_close(
                        lower[i],
                        expected.map(|(l, _, _)| l),
                        &format!("{label}.lower"),
                        i,
                    );
                    assert_close(
                        middle[i],
                        expected.map(|(_, m, _)| m),
                        &format!("{label}.middle"),
                        i,
                    );
                    assert_close(
                        upper[i],
                        expected.map(|(_, _, u)| u),
                        &format!("{label}.upper"),
                        i,
                    );
                }
            }
        }
    }

    #[test]
    fn donchian_matches_market_include_current_true() {
        let n = 400;
        let series = deterministic_series(n);
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Donchian,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Channel { lower, upper } = result else {
                panic!("donchian must produce Channel");
            };
            let expected =
                market::donchian_channel(&highs_dec[..=i], &lows_dec[..=i], period, true);
            assert_close(lower[i], expected.map(|(l, _)| l), "donchian.lower", i);
            assert_close(upper[i], expected.map(|(_, u)| u), "donchian.upper", i);
        }
    }

    #[test]
    fn keltner_matches_market_within_tolerance_and_keys_map_to_spec_names() {
        let n = 400;
        let series = deterministic_series(n);
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        let closes_dec = to_decimal(&series.close);
        let mult_dec = Decimal::from_f64(2.0).unwrap();
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Keltner,
                period: period as u32,
                mult_x100: 200,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Band {
                lower,
                middle,
                upper,
            } = result
            else {
                panic!("keltner must produce Band");
            };
            let expected = market::keltner_channels(
                &highs_dec[..=i],
                &lows_dec[..=i],
                &closes_dec[..=i],
                period,
                mult_dec,
            );
            assert_close(lower[i], expected.map(|(l, _, _)| l), "keltner.lower", i);
            assert_close(middle[i], expected.map(|(_, m, _)| m), "keltner.middle", i);
            assert_close(upper[i], expected.map(|(_, _, u)| u), "keltner.upper", i);
        }
    }

    // ---- IndicatorCache --------------------------------------------------

    fn single_series(v: Vec<f64>) -> IndicatorSeries {
        IndicatorSeries::Single(v)
    }

    #[test]
    fn get_or_compute_returns_the_same_arc_for_the_same_key() {
        let cache = IndicatorCache::new(512);
        let key = IndicatorKey {
            tf: Tf::M5,
            kind: IndicatorKind::Sma,
            period: 10,
            mult_x100: 0,
        };
        let first = cache.get_or_compute(key, || single_series(vec![1.0, 2.0, 3.0]));
        let second = cache.get_or_compute(key, || single_series(vec![9.0, 9.0, 9.0]));
        assert!(
            Arc::ptr_eq(&first, &second),
            "second call must return the cached Arc, not recompute"
        );
    }

    #[test]
    fn concurrent_requests_for_the_same_key_compute_only_once() {
        let cache = Arc::new(IndicatorCache::new(512));
        let key = IndicatorKey {
            tf: Tf::M5,
            kind: IndicatorKind::Sma,
            period: 10,
            mult_x100: 0,
        };
        let compute_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let cache = cache.clone();
                let compute_count = compute_count.clone();
                std::thread::spawn(move || {
                    cache.get_or_compute(key, || {
                        compute_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        // 計算に時間がかかる状況を模し、他スレッドが追いついて同じキーを
                        // 要求する余地を作る。
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        single_series(vec![42.0])
                    })
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread should not panic");
        }

        assert_eq!(
            compute_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the same key must be computed exactly once across concurrent requests"
        );
    }

    #[test]
    fn eviction_drops_the_least_recently_used_key_and_recompute_matches_original() {
        // 1 MB の予算に対し、十分に大きい系列を複数キー分要求して破棄を起こす。
        let cache = IndicatorCache::new(1);
        let big = vec![1.23456_f64; 50_000]; // 50,000 * 8 bytes ≈ 390 KiB
        let key_of = |period: u32| IndicatorKey {
            tf: Tf::M5,
            kind: IndicatorKind::Sma,
            period,
            mult_x100: 0,
        };

        let first_value = cache.get_or_compute(key_of(1), || single_series(big.clone()));
        // 1 MB を超えさせるため、異なるキーを複数要求する(1 件あたり約 390 KiB → 4 件目で 1 MB 超)。
        for period in 2..=6 {
            cache.get_or_compute(key_of(period), || single_series(big.clone()));
        }

        // key_of(1) は最初に使われたキーなので、容量超過後は破棄されているはず。
        // 再要求すると計算し直され、元の値と一致する(値のみで判定。破棄で性能は変わるが結果は
        // 変わらないことの確認)。
        let recomputed = cache.get_or_compute(key_of(1), || single_series(big.clone()));
        match (&*first_value, &*recomputed) {
            (IndicatorSeries::Single(a), IndicatorSeries::Single(b)) => assert_eq!(a, b),
            _ => panic!("unexpected indicator series variant"),
        }
        assert!(
            !Arc::ptr_eq(&first_value, &recomputed),
            "key_of(1) should have been evicted and recomputed, not reused from cache"
        );
    }

    fn sma_key(period: u32) -> IndicatorKey {
        IndicatorKey {
            tf: Tf::M5,
            kind: IndicatorKind::Sma,
            period,
            mult_x100: 0,
        }
    }

    /// `used_bytes == 全エントリの bytes の合計` の不変条件を検証する。
    fn assert_used_bytes_matches_entries(cache: &IndicatorCache, context: &str) {
        let guard = cache.inner.lock().expect("test cache mutex");
        let sum: usize = guard.map.values().map(|e| e.bytes).sum();
        assert_eq!(
            guard.used_bytes, sum,
            "used_bytes must equal the sum of entry bytes ({context})"
        );
    }

    /// 並行テストが「ハングではなく失敗」になるための待ち時間の上限。
    const CONCURRENCY_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// キー A（`sma_key(1)`）の計算を、テストが解放するまで止めた状態にし、その間に
    /// `other` を別スレッドで実行して完了することを確かめる。判定は実時間の閾値ではなく
    /// 順序による: 「`other` の完了」が「A の解放」より前に起きなければ失敗する。
    /// 実装が全体の `Mutex` を計算中に保持していると `other` が完了しないため、
    /// `recv_timeout` が切れて明確なメッセージで失敗する（ハングしない）。
    fn assert_other_work_completes_while_key_a_is_computing(
        cache: Arc<IndicatorCache>,
        other: impl FnOnce(&IndicatorCache) + Send + 'static,
    ) {
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let slow = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                cache.get_or_compute(sma_key(1), || {
                    started_tx.send(()).expect("main thread is waiting");
                    // 解放が来ないまま 10 秒たったら、テスト側が失敗扱いにするので続行してよい。
                    let _ = release_rx.recv_timeout(CONCURRENCY_TEST_TIMEOUT);
                    single_series(vec![2.0])
                })
            })
        };
        started_rx
            .recv_timeout(CONCURRENCY_TEST_TIMEOUT)
            .expect("key A's computation did not start within the timeout");

        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let other_thread = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                other(&cache);
                let _ = done_tx.send(());
            })
        };
        let outcome = done_rx.recv_timeout(CONCURRENCY_TEST_TIMEOUT);

        // 成否にかかわらず A を解放し、スレッドを必ず終わらせてから判定する。
        release_tx.send(()).expect("key A's computation is waiting");
        slow.join().expect("slow worker should not panic");
        other_thread.join().expect("other worker should not panic");
        outcome.expect(
            "other work did not finish while key A was still computing: \
             the cache is blocking on key A's computation",
        );
    }

    #[test]
    fn a_slow_computation_for_one_key_does_not_block_a_cache_hit_for_another_key() {
        let cache = Arc::new(IndicatorCache::new(512));
        cache.get_or_compute(sma_key(2), || single_series(vec![1.0]));

        assert_other_work_completes_while_key_a_is_computing(cache, |cache| {
            cache.get_or_compute(sma_key(2), || panic!("key B is cached, must not recompute"));
        });
    }

    #[test]
    fn a_computation_for_one_key_does_not_block_a_computation_for_another_key() {
        let cache = Arc::new(IndicatorCache::new(512));

        assert_other_work_completes_while_key_a_is_computing(cache, |cache| {
            cache.get_or_compute(sma_key(3), || single_series(vec![1.0]));
        });
    }

    #[test]
    fn a_series_larger_than_the_budget_is_returned_but_not_retained() {
        let cache = IndicatorCache::new(1);
        let oversized = vec![1.0_f64; 200_000]; // 1.6 MB > 1 MiB の予算
        let compute_count = std::cell::Cell::new(0_usize);

        let first = cache.get_or_compute(sma_key(1), || {
            compute_count.set(compute_count.get() + 1);
            single_series(oversized.clone())
        });
        match &*first {
            IndicatorSeries::Single(v) => assert_eq!(v.len(), 200_000),
            _ => panic!("unexpected indicator series variant"),
        }
        {
            let guard = cache.inner.lock().expect("test cache mutex");
            assert!(
                !guard.map.contains_key(&sma_key(1)),
                "an oversized series must not stay in the cache"
            );
            assert_eq!(guard.used_bytes, 0, "used_bytes must not exceed the budget");
        }
        assert_used_bytes_matches_entries(&cache, "after oversized request");

        cache.get_or_compute(sma_key(1), || {
            compute_count.set(compute_count.get() + 1);
            single_series(oversized.clone())
        });
        assert_eq!(
            compute_count.get(),
            2,
            "an oversized series is not cached, so the next request recomputes"
        );
    }

    #[test]
    fn a_panicking_computation_can_be_retried_and_leaves_the_cache_consistent() {
        let cache = IndicatorCache::new(512);
        let compute_count = std::cell::Cell::new(0_usize);

        let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.get_or_compute(sma_key(1), || {
                compute_count.set(compute_count.get() + 1);
                panic!("intentional panic in compute_fn (expected by this test)")
            })
        }));
        assert!(first.is_err(), "the first request must propagate the panic");

        let second = cache.get_or_compute(sma_key(1), || {
            compute_count.set(compute_count.get() + 1);
            single_series(vec![7.0])
        });
        match &*second {
            IndicatorSeries::Single(v) => assert_eq!(v, &vec![7.0]),
            _ => panic!("unexpected indicator series variant"),
        }
        assert_eq!(compute_count.get(), 2, "the retry must recompute once");
        assert!(
            cache.inner.lock().is_ok(),
            "a panic in compute_fn must not poison the cache mutex"
        );
        assert_used_bytes_matches_entries(&cache, "after panic and retry");
    }

    #[test]
    fn used_bytes_equals_the_sum_of_entry_bytes_after_every_operation() {
        let cache = IndicatorCache::new(1);
        let big = vec![1.0_f64; 50_000]; // 400,000 bytes
        assert_used_bytes_matches_entries(&cache, "empty cache");
        for period in 1..=8 {
            cache.get_or_compute(sma_key(period), || single_series(big.clone()));
            assert_used_bytes_matches_entries(&cache, &format!("after insert {period}"));
            // ヒットでも不変条件が崩れないこと。
            cache.get_or_compute(sma_key(period), || single_series(big.clone()));
            assert_used_bytes_matches_entries(&cache, &format!("after hit {period}"));
        }
    }

    #[test]
    fn a_cache_hit_protects_the_key_from_being_evicted_first() {
        // 予算 1 MiB、1 件 400,000 バイト: 2 件までは収まり、3 件目で超過する。
        let cache = IndicatorCache::new(1);
        let big = vec![1.0_f64; 50_000];
        let key1 = cache.get_or_compute(sma_key(1), || single_series(big.clone()));
        cache.get_or_compute(sma_key(2), || single_series(big.clone()));
        // key 1 を使い直す。LRU は key 2 になる。
        cache.get_or_compute(sma_key(1), || panic!("key 1 is cached, must not recompute"));
        cache.get_or_compute(sma_key(3), || single_series(big.clone()));

        let key1_again = cache.get_or_compute(sma_key(1), || {
            panic!("key 1 was used recently, must survive")
        });
        assert!(Arc::ptr_eq(&key1, &key1_again));
        let recomputed = std::cell::Cell::new(false);
        cache.get_or_compute(sma_key(2), || {
            recomputed.set(true);
            single_series(big.clone())
        });
        assert!(
            recomputed.get(),
            "key 2 was least recently used and must have been evicted"
        );
        assert_used_bytes_matches_entries(&cache, "after eviction");
    }

    #[test]
    fn nothing_is_evicted_while_within_budget() {
        let cache = IndicatorCache::new(1);
        let big = vec![1.0_f64; 50_000]; // 2 件 = 800,000 バイト < 1 MiB
        let first = cache.get_or_compute(sma_key(1), || single_series(big.clone()));
        let second = cache.get_or_compute(sma_key(2), || single_series(big.clone()));

        let first_again = cache.get_or_compute(sma_key(1), || panic!("must not be evicted"));
        let second_again = cache.get_or_compute(sma_key(2), || panic!("must not be evicted"));
        assert!(Arc::ptr_eq(&first, &first_again));
        assert!(Arc::ptr_eq(&second, &second_again));
        assert_eq!(cache.inner.lock().expect("test cache mutex").map.len(), 2);
    }
}
