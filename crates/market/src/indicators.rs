use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

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

#[cfg(test)]
mod tests {
    use super::*;
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
        let prices: Vec<Decimal> = (0..=14).map(|i| Decimal::from(i)).collect();
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
}
