use auto_trader_core::types::{Exchange, Pair};
use chrono::{TimeZone, Utc};
use rust_decimal_macros::dec;

use super::*;

#[test]
fn emit_candle_event_populates_indicators_for_primary_timeframe() {
    let pair = Pair::new("FX_BTC_JPY");
    let mut closes_map: HashMap<String, Vec<Decimal>> = HashMap::new();
    let mut highs_map: HashMap<String, Vec<Decimal>> = HashMap::new();
    let mut lows_map: HashMap<String, Vec<Decimal>> = HashMap::new();

    let entry = closes_map.entry("FX_BTC_JPY".to_string()).or_default();
    for i in 0..20u64 {
        entry.push(Decimal::from(10_000_000u64 + i * 1000));
    }
    highs_map
        .entry("FX_BTC_JPY".to_string())
        .or_default()
        .extend(entry.iter().map(|c| *c + dec!(5000)));
    lows_map
        .entry("FX_BTC_JPY".to_string())
        .or_default()
        .extend(entry.iter().map(|c| *c - dec!(5000)));

    let candle = auto_trader_core::types::Candle {
        pair: pair.clone(),
        exchange: Exchange::BitflyerCfd,
        timeframe: "M5".to_string(),
        open: dec!(10_020_000),
        high: dec!(10_025_000),
        low: dec!(10_015_000),
        close: dec!(10_022_000),
        volume: Some(10),
        best_bid: None,
        best_ask: None,
        timestamp: Utc.with_ymd_and_hms(2026, 4, 19, 0, 5, 0).unwrap(),
    };

    let (event, _indicators) =
        emit_candle_event(candle, &mut closes_map, &mut highs_map, &mut lows_map, true);
    assert_eq!(event.candle.timeframe, "M5");
    assert!(
        event.indicators.contains_key("sma_20"),
        "sma_20 must be in indicator map"
    );
}

type CloseHighLowSeries = (
    HashMap<String, Vec<Decimal>>,
    HashMap<String, Vec<Decimal>>,
    HashMap<String, Vec<Decimal>>,
);

fn build_indicator_series(code: &str, base_at: impl Fn(u64) -> Decimal) -> CloseHighLowSeries {
    let mut closes: HashMap<String, Vec<Decimal>> = HashMap::new();
    let mut highs: HashMap<String, Vec<Decimal>> = HashMap::new();
    let mut lows: HashMap<String, Vec<Decimal>> = HashMap::new();
    {
        let c = closes.entry(code.to_string()).or_default();
        let h = highs.entry(code.to_string()).or_default();
        let l = lows.entry(code.to_string()).or_default();
        for i in 0..40u64 {
            let base = base_at(i);
            c.push(base);
            h.push(base + dec!(3000));
            l.push(base - dec!(3000));
        }
    }
    (closes, highs, lows)
}

fn m5_candle_with_indicators(pair: &Pair, code: &str) -> (PriceEvent, HashMap<String, Decimal>) {
    let (mut closes, mut highs, mut lows) =
        build_indicator_series(code, |i| Decimal::from(10_000_000u64 + i * 20_000));
    let candle = auto_trader_core::types::Candle {
        pair: pair.clone(),
        exchange: Exchange::BitflyerCfd,
        timeframe: "M5".to_string(),
        open: dec!(10_800_000),
        high: dec!(10_823_000),
        low: dec!(10_817_000),
        close: dec!(10_820_000),
        volume: Some(10),
        best_bid: None,
        best_ask: None,
        timestamp: Utc.with_ymd_and_hms(2026, 4, 19, 0, 5, 0).unwrap(),
    };
    emit_candle_event(candle, &mut closes, &mut highs, &mut lows, true)
}

fn h1_candle_with_indicators(pair: &Pair, code: &str) -> (PriceEvent, HashMap<String, Decimal>) {
    let (mut closes, mut highs, mut lows) = build_indicator_series(code, |i| {
        let osc = if i % 2 == 0 { 50_000 } else { 0 };
        Decimal::from(10_000_000u64 + osc)
    });
    let candle = auto_trader_core::types::Candle {
        pair: pair.clone(),
        exchange: Exchange::BitflyerCfd,
        timeframe: "H1".to_string(),
        open: dec!(10_000_000),
        high: dec!(10_053_000),
        low: dec!(9_997_000),
        close: dec!(10_050_000),
        volume: Some(10),
        best_bid: None,
        best_ask: None,
        timestamp: Utc.with_ymd_and_hms(2026, 4, 19, 1, 0, 0).unwrap(),
    };
    emit_candle_event(candle, &mut closes, &mut highs, &mut lows, true)
}

#[test]
fn h1_event_carries_h1_native_indicators_not_m5() {
    let pair = Pair::new("FX_BTC_JPY");
    let code = "FX_BTC_JPY";

    let (_m5_event, m5_indicators) = m5_candle_with_indicators(&pair, code);
    let (mut h1_event, _h1_ind) = h1_candle_with_indicators(&pair, code);
    for (key, value) in &m5_indicators {
        h1_event.indicators.insert(format!("m5_{key}"), *value);
    }

    let h1_native_adx = *h1_event
        .indicators
        .get("adx_14")
        .expect("H1 event must carry a bare adx_14");
    let m5_adx_prefixed = *h1_event
        .indicators
        .get("m5_adx_14")
        .expect("H1 event must also carry m5_adx_14 (prefixed)");
    let m5_native = *m5_indicators
        .get("adx_14")
        .expect("M5 map must have adx_14");

    assert_eq!(h1_event.candle.timeframe, "H1");
    assert_eq!(
        m5_adx_prefixed, m5_native,
        "m5_adx_14 must equal the M5-native ADX"
    );
    assert_ne!(
        h1_native_adx, m5_adx_prefixed,
        "bare adx_14 must be H1-native, not the M5 value"
    );
}
