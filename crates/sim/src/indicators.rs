use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::series::{Tf, TfSeries};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndicatorKey {
    pub tf: Tf,
    pub kind: IndicatorKind,
    pub period: u32,
    pub mult_x100: u32,
}

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
    fn estimate_bytes_by_element_count_since_eviction_needs_only_rough_sizing(&self) -> usize {
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

fn zero_sum_exactly_when_its_count_hits_zero_so_f64_residue_cannot_fake_a_nonzero_average(
    sum: f64,
    count: usize,
) -> f64 {
    if count == 0 { 0.0 } else { sum }
}

fn rsi_series(closes: &[f64], period: usize) -> Vec<f64> {
    let n = closes.len();
    let mut result = vec![f64::NAN; n];
    if period == 0 || n < period + 1 {
        return result;
    }
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
        gain_sum =
            zero_sum_exactly_when_its_count_hits_zero_so_f64_residue_cannot_fake_a_nonzero_average(
                gain_sum, gain_count,
            );
        loss_sum =
            zero_sum_exactly_when_its_count_hits_zero_so_f64_residue_cannot_fake_a_nonzero_average(
                loss_sum, loss_count,
            );
        let bar_index = k + 1;
        if bar_index >= period {
            let avg_gain = gain_sum / period as f64;
            let avg_loss = loss_sum / period as f64;
            result[bar_index] = if avg_loss == 0.0 {
                100.0
            } else {
                let rs = avg_gain / avg_loss;
                100.0 - 100.0 / (1.0 + rs)
            };
        }
    }
    result
}

fn true_range(highs: &[f64], lows: &[f64], closes: &[f64], k: usize) -> f64 {
    let hl = highs[k] - lows[k];
    let hc = (highs[k] - closes[k - 1]).abs();
    let lc = (lows[k] - closes[k - 1]).abs();
    hl.max(hc).max(lc)
}

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

fn includes_in_dx_only_when_smooth_tr_is_nonzero_to_match_markets_skip(smooth_tr: f64) -> bool {
    smooth_tr != 0.0
}

fn adx_series(highs: &[f64], lows: &[f64], closes: &[f64], period: usize) -> Vec<f64> {
    let n = closes.len();
    let mut result = vec![f64::NAN; n];
    if period == 0 || n < period * 2 + 1 {
        return result;
    }
    let p = period as f64;
    let pair_count = n - 1;
    let mut plus_dm = vec![0.0f64; pair_count];
    let mut minus_dm = vec![0.0f64; pair_count];
    let mut tr = vec![0.0f64; pair_count];
    for j in 0..pair_count {
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

    for j in period..pair_count {
        smooth_plus = smooth_plus - smooth_plus / p + plus_dm[j];
        smooth_minus = smooth_minus - smooth_minus / p + minus_dm[j];
        smooth_tr = smooth_tr - smooth_tr / p + tr[j];
        let bar_index = j + 1;

        if includes_in_dx_only_when_smooth_tr_is_nonzero_to_match_markets_skip(smooth_tr) {
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

fn compute_window_variance_directly_since_the_sum_of_squares_shortcut_loses_precision_at_scale(
    window: &[f64],
    mean: f64,
    p: f64,
) -> f64 {
    window
        .iter()
        .map(|&x| {
            let diff = x - mean;
            diff * diff
        })
        .sum::<f64>()
        / p
}

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
            let variance =
                compute_window_variance_directly_since_the_sum_of_squares_shortcut_loses_precision_at_scale(
                    window, mean, p,
                );
            let band = variance.sqrt() * mult;
            lower[i] = mean - band;
            middle[i] = mean;
            upper[i] = mean + band;
        }
    }
    (lower, middle, upper)
}

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

#[derive(Debug)]
pub(crate) struct IndicatorCache {
    budget_bytes: usize,
    inner: Mutex<CacheInner>,
}

type Slot = Arc<OnceLock<Arc<IndicatorSeries>>>;

#[derive(Debug)]
struct CacheEntry {
    slot: Slot,
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

    pub(crate) fn get_or_compute(
        &self,
        key: IndicatorKey,
        compute_fn: impl FnOnce() -> IndicatorSeries,
    ) -> Arc<IndicatorSeries> {
        let slot =
            match self.claim_slot_or_cached_series_without_holding_the_mutex_during_compute(key) {
                Ok(series) => return series,
                Err(slot) => slot,
            };

        let series = slot.get_or_init(|| Arc::new(compute_fn())).clone();

        self.register_bytes_then_drop_entry_that_alone_exceeds_the_budget(key, &slot, &series);
        series
    }

    fn claim_slot_or_cached_series_without_holding_the_mutex_during_compute(
        &self,
        key: IndicatorKey,
    ) -> Result<Arc<IndicatorSeries>, Slot> {
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
            return Ok(series.clone());
        }
        Err(entry.slot.clone())
    }

    fn register_bytes_then_drop_entry_that_alone_exceeds_the_budget(
        &self,
        key: IndicatorKey,
        slot: &Slot,
        series: &Arc<IndicatorSeries>,
    ) {
        let mut guard = self.inner.lock().expect(POISONED_MESSAGE);
        let registered_bytes = match guard.map.get_mut(&key) {
            Some(entry) if Arc::ptr_eq(&entry.slot, slot) && entry.bytes == 0 => {
                entry.bytes =
                    series.estimate_bytes_by_element_count_since_eviction_needs_only_rough_sizing();
                entry.bytes
            }
            _ => 0,
        };
        guard.used_bytes += registered_bytes;
        if registered_bytes == 0 {
            return;
        }
        self.evict_over_budget(&mut guard, key);
        let alone_exceeds_budget =
            guard.used_bytes > self.budget_bytes && registered_bytes > self.budget_bytes;
        if !alone_exceeds_budget {
            return;
        }
        let is_ours = guard
            .map
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.slot, slot));
        let removed = if is_ours {
            guard.map.remove(&key)
        } else {
            None
        };
        if let Some(removed) = removed {
            guard.used_bytes -= removed.bytes;
        }
    }

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
mod tests;
