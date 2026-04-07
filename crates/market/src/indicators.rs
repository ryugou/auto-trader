use rust_decimal::Decimal;

pub fn sma(prices: &[Decimal], period: usize) -> Option<Decimal> {
    if prices.len() < period {
        return None;
    }
    let sum: Decimal = prices[prices.len() - period..].iter().sum();
    Some(sum / Decimal::from(period as u64))
}

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

/// Bollinger Bands. Returns (lower, middle, upper).
///
/// `std_dev_mult` is typically 2.0 (textbook). Strategies that want a wider
/// extreme can use 2.5 to reduce false signals on a noisy crypto pair.
///
/// Population (not sample) standard deviation, matching most trading
/// platforms (TradingView, MetaTrader).
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
    // Decimal lacks sqrt; use f64 for the variance step. Acceptable for
    // indicator purposes (price distances are tiny relative to f64 precision).
    use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
    let std_dev_f64 = variance.to_f64()?.sqrt();
    let std_dev = Decimal::from_f64(std_dev_f64)?;
    let band = std_dev * std_dev_mult;
    Some((middle - band, middle, middle + band))
}

/// Average True Range over `period` bars. Wilder's smoothing (the canonical
/// definition) — uses the rolling running average for periods past the first.
///
/// Inputs are aligned slices: `highs[i]`, `lows[i]`, `closes[i]` describe the
/// same bar. The first ATR value is computed at index `period`, so caller
/// must pass at least `period + 1` bars.
pub fn atr(highs: &[Decimal], lows: &[Decimal], closes: &[Decimal], period: usize) -> Option<Decimal> {
    if highs.len() != lows.len() || lows.len() != closes.len() {
        return None;
    }
    if highs.len() < period + 1 {
        return None;
    }
    let n = highs.len();
    // True Range for each bar i (i >= 1):
    //   TR = max(high - low, |high - prev_close|, |low - prev_close|)
    let mut trs = Vec::with_capacity(n - 1);
    for i in 1..n {
        let hl = highs[i] - lows[i];
        let hc = (highs[i] - closes[i - 1]).abs();
        let lc = (lows[i] - closes[i - 1]).abs();
        trs.push(hl.max(hc).max(lc));
    }
    // First ATR = simple average of first `period` TR values.
    let mut atr_val = trs[..period].iter().sum::<Decimal>() / Decimal::from(period as u64);
    // Wilder's smoothing for the rest.
    let n_dec = Decimal::from(period as u64);
    for tr in &trs[period..] {
        atr_val = (atr_val * (n_dec - Decimal::ONE) + *tr) / n_dec;
    }
    Some(atr_val)
}

/// Donchian Channel. Returns (lower, upper) — the rolling min low and max
/// high over `period` bars (excluding the most recent bar so the breakout
/// is clean — see `include_current` arg).
///
/// `include_current = false` is the textbook Turtle definition: "20-day
/// breakout" means the high is greater than the prior 20 bars' high, not
/// the prior 19 bars' + today.
pub fn donchian_channel(
    highs: &[Decimal],
    lows: &[Decimal],
    period: usize,
    include_current: bool,
) -> Option<(Decimal, Decimal)> {
    if highs.len() != lows.len() {
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

/// Keltner Channels. Returns (lower, middle, upper).
///
/// Uses EMA(period) of closes as the middle line and ATR(period) × `atr_mult`
/// as the band width. Standard parameters for the TTM Squeeze are
/// `period = 20`, `atr_mult = 1.5`.
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
mod tests {
    use super::*;
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal_macros::dec;

    #[test]
    fn sma_basic() {
        let prices = vec![dec!(1), dec!(2), dec!(3), dec!(4), dec!(5)];
        assert_eq!(sma(&prices, 3), Some(dec!(4))); // (3+4+5)/3
    }

    #[test]
    fn sma_insufficient_data() {
        let prices = vec![dec!(1), dec!(2)];
        assert_eq!(sma(&prices, 3), None);
    }

    #[test]
    fn rsi_all_gains() {
        let prices: Vec<Decimal> = (0..=14).map(Decimal::from).collect();
        let result = rsi(&prices, 14).unwrap();
        assert_eq!(result, dec!(100));
    }

    #[test]
    fn rsi_mixed() {
        let prices = vec![
            dec!(44), dec!(44.34), dec!(44.09), dec!(43.61), dec!(44.33),
            dec!(44.83), dec!(45.10), dec!(45.42), dec!(45.84), dec!(46.08),
            dec!(45.89), dec!(46.03), dec!(45.61), dec!(46.28), dec!(46.28),
        ];
        let result = rsi(&prices, 14).unwrap();
        let f = result.to_f64().unwrap();
        assert!(f > 60.0 && f < 80.0, "RSI should be ~70, got {f}");
    }

    #[test]
    fn bollinger_bands_centers_on_sma_and_widens_with_volatility() {
        // Constant prices: bands should equal middle (zero stddev)
        let flat = vec![dec!(100); 20];
        let (lo, mid, up) = bollinger_bands(&flat, 20, dec!(2)).unwrap();
        assert_eq!(mid, dec!(100));
        assert_eq!(lo, dec!(100));
        assert_eq!(up, dec!(100));

        // Alternating prices: nonzero band width, symmetric around mean
        let alt: Vec<Decimal> = (0..20)
            .map(|i| if i % 2 == 0 { dec!(100) } else { dec!(102) })
            .collect();
        let (lo, mid, up) = bollinger_bands(&alt, 20, dec!(2)).unwrap();
        assert_eq!(mid, dec!(101)); // (100*10 + 102*10) / 20
        // population stddev of {100,102,100,102,...} = 1.0
        // band = 2 * 1 = 2
        let lo_f = lo.to_f64().unwrap();
        let up_f = up.to_f64().unwrap();
        assert!((lo_f - 99.0).abs() < 0.01);
        assert!((up_f - 103.0).abs() < 0.01);
    }

    #[test]
    fn bollinger_bands_insufficient_data_returns_none() {
        let prices = vec![dec!(1); 5];
        assert!(bollinger_bands(&prices, 20, dec!(2)).is_none());
    }

    #[test]
    fn atr_basic_constant_range() {
        // Constant 10-wide range each bar with no gaps → ATR == 10
        let highs: Vec<Decimal> = (0..20).map(|_| dec!(110)).collect();
        let lows: Vec<Decimal> = (0..20).map(|_| dec!(100)).collect();
        let closes: Vec<Decimal> = (0..20).map(|_| dec!(105)).collect();
        let v = atr(&highs, &lows, &closes, 14).unwrap();
        assert_eq!(v, dec!(10));
    }

    #[test]
    fn atr_includes_gap_in_true_range() {
        // Bar 0: [100, 110], close 105
        // Bar 1: [120, 130], close 125  → TR = max(10, |130-105|=25, |120-105|=15) = 25
        let highs = vec![dec!(110), dec!(130)];
        let lows = vec![dec!(100), dec!(120)];
        let closes = vec![dec!(105), dec!(125)];
        // period=1: first ATR = TR[0] = 25
        let v = atr(&highs, &lows, &closes, 1).unwrap();
        assert_eq!(v, dec!(25));
    }

    #[test]
    fn donchian_channel_excludes_current_by_default() {
        let highs = vec![dec!(10), dec!(20), dec!(15), dec!(50)];
        let lows = vec![dec!(5), dec!(8), dec!(7), dec!(40)];
        // include_current = false: window is bars [0..3], excludes bar 3
        let (lo, up) = donchian_channel(&highs, &lows, 3, false).unwrap();
        assert_eq!(lo, dec!(5));
        assert_eq!(up, dec!(20));

        // include_current = true: window is last 3 bars [1..4]
        let (lo, up) = donchian_channel(&highs, &lows, 3, true).unwrap();
        assert_eq!(lo, dec!(7));
        assert_eq!(up, dec!(50));
    }

    #[test]
    fn keltner_channels_widen_with_atr() {
        // Steady upward 1-per-bar trend, range 1 each bar.
        // True Range each bar accounts for gap to previous close:
        //   high[i] - close[i-1] = (close[i] + 0.5) - close[i-1] = 1.5
        // So ATR converges to ~1.5, band = 1.5 * 1.5 = 2.25, width ≈ 4.5
        let closes: Vec<Decimal> = (0..20).map(|i| dec!(100) + Decimal::from(i)).collect();
        let highs: Vec<Decimal> = closes.iter().map(|c| *c + dec!(0.5)).collect();
        let lows: Vec<Decimal> = closes.iter().map(|c| *c - dec!(0.5)).collect();
        let (lo, mid, up) = keltner_channels(&highs, &lows, &closes, 14, dec!(1.5)).unwrap();
        // Middle is EMA, should be near the recent average
        assert!(mid > dec!(100) && mid < dec!(120));
        assert!(lo < mid && mid < up);
        let band_width = up - lo;
        let f = band_width.to_f64().unwrap();
        assert!(f > 4.0 && f < 5.0, "expected band width ~4.5, got {f}");
    }
}
