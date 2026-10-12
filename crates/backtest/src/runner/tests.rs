use chrono::{Duration, TimeZone, Utc};
use rust_decimal::Decimal;

use super::*;

const SERIES_LEN: usize = 200;

fn series() -> Vec<Candle> {
    let base = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
    (0..SERIES_LEN)
        .map(|n| {
            let close = Decimal::from(1000 + (n * 37 % 91) as i64);
            Candle {
                pair: Pair::new("FX_BTC_JPY"),
                exchange: Exchange::BitflyerCfd,
                timeframe: "H1".to_string(),
                open: close,
                high: close,
                low: close,
                close,
                volume: Some(1),
                best_bid: None,
                best_ask: None,
                timestamp: base + Duration::hours(n as i64),
            }
        })
        .collect()
}

#[test]
fn recent_closes_never_exceeds_lookback_window() {
    let candles = series();
    for i in [0, 10, 49, 50, 100, SERIES_LEN - 1] {
        let closes = recent_closes(&candles, i);
        assert!(closes.len() <= INDICATOR_LOOKBACK_CANDLES, "i={i}");
        assert_eq!(
            closes.len(),
            (i + 1).min(INDICATOR_LOOKBACK_CANDLES),
            "i={i}"
        );
        assert_eq!(closes.last(), Some(&candles[i].close), "i={i}");
    }
}

#[test]
fn indicators_up_to_matches_full_history_calculation() {
    let candles = series();
    for i in [5, 19, 20, 49, 50, 51, 100, SERIES_LEN - 1] {
        let full: Vec<Decimal> = candles[..=i].iter().map(|c| c.close).collect();
        let mut expected = HashMap::new();
        if let Some(v) = auto_trader_market::indicators::sma(&full, 20) {
            expected.insert("sma_20".to_string(), v);
        }
        if let Some(v) = auto_trader_market::indicators::sma(&full, 50) {
            expected.insert("sma_50".to_string(), v);
        }
        if let Some(v) = auto_trader_market::indicators::rsi(&full, 14) {
            expected.insert("rsi_14".to_string(), v);
        }
        assert_eq!(indicators_up_to(&candles, i), expected, "i={i}");
    }
}
