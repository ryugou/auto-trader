use std::collections::HashMap;

use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};

pub(crate) fn indicator_map(
    closes: &[Decimal],
    highs: &[Decimal],
    lows: &[Decimal],
) -> HashMap<String, Decimal> {
    let mut map = HashMap::new();
    if let Some(v) = sma(closes, 20) {
        map.insert("sma_20".to_string(), v);
    }
    if let Some(v) = sma(closes, 50) {
        map.insert("sma_50".to_string(), v);
    }
    if let Some(v) = rsi(closes, 14) {
        map.insert("rsi_14".to_string(), v);
    }
    if let Some(v) = atr(highs, lows, closes, 14) {
        map.insert("atr_14".to_string(), v);
    }
    if let Some(v) = adx(highs, lows, closes, 14) {
        map.insert("adx_14".to_string(), v);
    }
    if let Some((bb_lo, bb_mid, bb_up)) = bollinger_bands(closes, 20, Decimal::from(2))
        && bb_mid > Decimal::ZERO
    {
        let bb_width_pct = (bb_up - bb_lo) / bb_mid * Decimal::from(100);
        map.insert("bb_width_pct".to_string(), bb_width_pct);
    }
    if let Some(current_atr) = map.get("atr_14").copied() {
        let lookback = 50.min(closes.len());
        if lookback >= 15 {
            let mut atr_count_below = 0u32;
            let mut atr_total = 0u32;
            for end in (closes.len() - lookback)..closes.len() {
                if end >= 14
                    && let Some(past_atr) = atr(&highs[..=end], &lows[..=end], &closes[..=end], 14)
                {
                    atr_total += 1;
                    if past_atr < current_atr {
                        atr_count_below += 1;
                    }
                }
            }
            if atr_total > 0 {
                let pct =
                    Decimal::from(atr_count_below) / Decimal::from(atr_total) * Decimal::from(100);
                map.insert("atr_percentile".to_string(), pct);
            }
        }
    }
    map
}

/// Returns the simple moving average of the last `period` prices, or `None` when there are fewer than `period` prices.
///
/// # Panics
///
/// Panics if `period` is zero.
pub fn sma(prices: &[Decimal], period: usize) -> Option<Decimal> {
    if prices.len() < period {
        return None;
    }
    let sum: Decimal = prices[prices.len() - period..].iter().sum();
    Some(sum / Decimal::from(period as u64))
}

/// Returns the exponential moving average of `prices` over `period`, seeded with the simple moving average of the first `period` prices, or `None` when there are fewer than `period` prices.
///
/// # Panics
///
/// Panics if `period` is zero.
pub fn ema(prices: &[Decimal], period: usize) -> Option<Decimal> {
    if prices.len() < period {
        return None;
    }
    let multiplier = Decimal::from(2) / Decimal::from((period + 1) as u64);
    let mut ema_val = sma(&prices[..period], period)?;
    for price in &prices[period..] {
        ema_val = (*price - ema_val) * multiplier + ema_val;
    }
    Some(ema_val)
}

/// Returns the relative strength index over the last `period` price changes, which is `100` when none of them is a loss, or `None` when there are fewer than `period + 1` prices.
///
/// # Panics
///
/// Panics if `period` is zero and `prices` is not empty.
pub fn rsi(prices: &[Decimal], period: usize) -> Option<Decimal> {
    if prices.len() < period + 1 {
        return None;
    }
    let changes: Vec<Decimal> = prices.windows(2).map(|w| w[1] - w[0]).collect();
    let recent = &changes[changes.len() - period..];
    let mut avg_gain = Decimal::ZERO;
    let mut avg_loss = Decimal::ZERO;
    for change in recent {
        if *change > Decimal::ZERO {
            avg_gain += change;
        } else {
            avg_loss += change.abs();
        }
    }
    avg_gain /= Decimal::from(period as u64);
    avg_loss /= Decimal::from(period as u64);
    if avg_loss == Decimal::ZERO {
        return Some(Decimal::from(100));
    }
    let rs = avg_gain / avg_loss;
    Some(Decimal::from(100) - Decimal::from(100) / (Decimal::ONE + rs))
}

/// Returns the lower band, the middle and the upper band, which are the simple moving average of the last `period` prices minus, at and plus `std_dev_mult` times their population standard deviation, or `None` when there are fewer than `period` prices or the deviation cannot be converted to a `Decimal`.
///
/// # Panics
///
/// Panics if `period` is zero.
pub fn bollinger_bands(
    prices: &[Decimal],
    period: usize,
    std_dev_mult: Decimal,
) -> Option<(Decimal, Decimal, Decimal)> {
    let middle = sma(prices, period)?;
    let window = &prices[prices.len() - period..];
    let n = Decimal::from(period as u64);
    let variance: Decimal = window
        .iter()
        .map(|p| {
            let diff = *p - middle;
            diff * diff
        })
        .sum::<Decimal>()
        / n;
    let std_dev_f64 = variance.to_f64()?.sqrt();
    let std_dev = Decimal::from_f64(std_dev_f64)?;
    let band = std_dev * std_dev_mult;
    Some((middle - band, middle, middle + band))
}

/// Returns the average true range over `period` with Wilder smoothing, or `None` when the slices differ in length or hold fewer than `period + 1` elements.
///
/// # Panics
///
/// Panics if `period` is zero and the slices have the same non-zero length.
pub fn atr(
    highs: &[Decimal],
    lows: &[Decimal],
    closes: &[Decimal],
    period: usize,
) -> Option<Decimal> {
    if highs.len() != lows.len() || lows.len() != closes.len() {
        return None;
    }
    if highs.len() < period + 1 {
        return None;
    }
    let n = highs.len();
    let mut trs = Vec::with_capacity(n - 1);
    for i in 1..n {
        let hl = highs[i] - lows[i];
        let hc = (highs[i] - closes[i - 1]).abs();
        let lc = (lows[i] - closes[i - 1]).abs();
        trs.push(hl.max(hc).max(lc));
    }
    let mut atr_val = trs[..period].iter().sum::<Decimal>() / Decimal::from(period as u64);
    let n_dec = Decimal::from(period as u64);
    for tr in &trs[period..] {
        atr_val = (atr_val * (n_dec - Decimal::ONE) + *tr) / n_dec;
    }
    Some(atr_val)
}

/// Returns the lowest low and the highest high of the last `period` candles as a pair, leaving out the last candle unless `include_current` is `true`, or `None` when the slices differ in length, are empty or hold fewer than `period` candles to look at.
pub fn donchian_channel(
    highs: &[Decimal],
    lows: &[Decimal],
    period: usize,
    include_current: bool,
) -> Option<(Decimal, Decimal)> {
    if highs.len() != lows.len() {
        return None;
    }
    if highs.is_empty() {
        return None;
    }
    let end = if include_current {
        highs.len()
    } else {
        highs.len() - 1
    };
    if end < period {
        return None;
    }
    let lower = *lows[end - period..end].iter().min()?;
    let upper = *highs[end - period..end].iter().max()?;
    Some((lower, upper))
}

/// Returns the average directional index over `period` with Wilder smoothing, or `None` when a slice has fewer than `2 * period + 1` elements or fewer than `period` directional index values can be computed.
///
/// # Panics
///
/// Panics if `lows` has fewer elements than `highs`, if `closes` has fewer than `highs` has minus one, or if `period` is zero and no slice is empty.
pub fn adx(
    highs: &[Decimal],
    lows: &[Decimal],
    closes: &[Decimal],
    period: usize,
) -> Option<Decimal> {
    if highs.len() < period * 2 + 1 || lows.len() < period * 2 + 1 || closes.len() < period * 2 + 1
    {
        return None;
    }
    let len = highs.len();
    let mut plus_dm_vals = Vec::with_capacity(len - 1);
    let mut minus_dm_vals = Vec::with_capacity(len - 1);
    let mut tr_vals = Vec::with_capacity(len - 1);
    for i in 1..len {
        let high_diff = highs[i] - highs[i - 1];
        let low_diff = lows[i - 1] - lows[i];
        let plus_dm = if high_diff > low_diff && high_diff > Decimal::ZERO {
            high_diff
        } else {
            Decimal::ZERO
        };
        let minus_dm = if low_diff > high_diff && low_diff > Decimal::ZERO {
            low_diff
        } else {
            Decimal::ZERO
        };
        let hl = highs[i] - lows[i];
        let hc = (highs[i] - closes[i - 1]).abs();
        let lc = (lows[i] - closes[i - 1]).abs();
        let tr = hl.max(hc).max(lc);
        plus_dm_vals.push(plus_dm);
        minus_dm_vals.push(minus_dm);
        tr_vals.push(tr);
    }
    if plus_dm_vals.len() < period * 2 {
        return None;
    }
    let p = Decimal::from(period as i64);
    let mut smooth_plus: Decimal = plus_dm_vals[..period].iter().sum();
    let mut smooth_minus: Decimal = minus_dm_vals[..period].iter().sum();
    let mut smooth_tr: Decimal = tr_vals[..period].iter().sum();

    let mut dx_vals = Vec::new();
    for i in period..plus_dm_vals.len() {
        smooth_plus = smooth_plus - smooth_plus / p + plus_dm_vals[i];
        smooth_minus = smooth_minus - smooth_minus / p + minus_dm_vals[i];
        smooth_tr = smooth_tr - smooth_tr / p + tr_vals[i];
        if smooth_tr == Decimal::ZERO {
            continue;
        }
        let plus_di = smooth_plus / smooth_tr * Decimal::from(100);
        let minus_di = smooth_minus / smooth_tr * Decimal::from(100);
        let di_sum = plus_di + minus_di;
        if di_sum == Decimal::ZERO {
            dx_vals.push(Decimal::ZERO);
        } else {
            let dx = ((plus_di - minus_di).abs() / di_sum) * Decimal::from(100);
            dx_vals.push(dx);
        }
    }
    if dx_vals.len() < period {
        return None;
    }
    let first_adx: Decimal = dx_vals[..period].iter().sum::<Decimal>() / p;
    let mut adx_val = first_adx;
    for dx in &dx_vals[period..] {
        adx_val = (adx_val * (p - Decimal::ONE) + dx) / p;
    }
    Some(adx_val)
}

/// Returns the lower band, the middle and the upper band, which are the exponential moving average of `closes` over `period` minus, at and plus `atr_mult` times the average true range over `period`, or `None` when either of them cannot be computed.
///
/// # Panics
///
/// Panics if `period` is zero.
pub fn keltner_channels(
    highs: &[Decimal],
    lows: &[Decimal],
    closes: &[Decimal],
    period: usize,
    atr_mult: Decimal,
) -> Option<(Decimal, Decimal, Decimal)> {
    let middle = ema(closes, period)?;
    let atr_val = atr(highs, lows, closes, period)?;
    let band = atr_val * atr_mult;
    Some((middle - band, middle, middle + band))
}

#[cfg(test)]
mod tests;
