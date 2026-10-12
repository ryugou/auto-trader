use auto_trader_core::types::{Exchange, Pair};
use chrono::{TimeZone, Utc};
use rust_decimal_macros::dec;

use super::*;
use crate::candle_builder::CandleBuilder;

#[test]
fn new_auto_adds_btc_jpy_when_fx_btc_jpy_is_subscribed() {
    let mon = BitflyerMonitor::new("ws://test", vec![Pair::new("FX_BTC_JPY")], "M5");
    let pair_codes: Vec<&str> = mon.pairs.iter().map(|p| p.0.as_str()).collect();
    assert!(pair_codes.contains(&"FX_BTC_JPY"));
    assert!(
        pair_codes.contains(&"BTC_JPY"),
        "BTC_JPY (spot) must be auto-added for SFD calc, got: {:?}",
        pair_codes
    );
}

#[test]
fn new_does_not_add_btc_jpy_when_no_fx_btc_jpy() {
    let mon = BitflyerMonitor::new("ws://test", vec![Pair::new("ETH_JPY")], "M5");
    let pair_codes: Vec<&str> = mon.pairs.iter().map(|p| p.0.as_str()).collect();
    assert!(!pair_codes.contains(&"BTC_JPY"));
}

#[test]
fn new_does_not_duplicate_btc_jpy_when_already_subscribed() {
    let mon = BitflyerMonitor::new(
        "ws://test",
        vec![Pair::new("FX_BTC_JPY"), Pair::new("BTC_JPY")],
        "M5",
    );
    let btc_count = mon.pairs.iter().filter(|p| p.0 == "BTC_JPY").count();
    assert_eq!(btc_count, 1, "BTC_JPY must not be duplicated");
}

#[test]
fn is_spot_only_classifies_btc_jpy_as_spot() {
    assert!(is_spot_only(&Pair::new("BTC_JPY")));
    assert!(!is_spot_only(&Pair::new("FX_BTC_JPY")));
    assert!(!is_spot_only(&Pair::new("ETH_JPY")));
}

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

#[test]
fn h1_event_carries_h1_native_indicators_not_m5() {
    let pair = Pair::new("FX_BTC_JPY");
    let code = "FX_BTC_JPY".to_string();

    let mut m5_closes: HashMap<String, Vec<Decimal>> = HashMap::new();
    let mut m5_highs: HashMap<String, Vec<Decimal>> = HashMap::new();
    let mut m5_lows: HashMap<String, Vec<Decimal>> = HashMap::new();
    {
        let c = m5_closes.entry(code.clone()).or_default();
        let h = m5_highs.entry(code.clone()).or_default();
        let l = m5_lows.entry(code.clone()).or_default();
        for i in 0..40u64 {
            let base = Decimal::from(10_000_000u64 + i * 20_000);
            c.push(base);
            h.push(base + dec!(3000));
            l.push(base - dec!(3000));
        }
    }
    let m5_candle = auto_trader_core::types::Candle {
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
    let (_m5_event, m5_indicators) =
        emit_candle_event(m5_candle, &mut m5_closes, &mut m5_highs, &mut m5_lows, true);

    let mut h1_closes: HashMap<String, Vec<Decimal>> = HashMap::new();
    let mut h1_highs: HashMap<String, Vec<Decimal>> = HashMap::new();
    let mut h1_lows: HashMap<String, Vec<Decimal>> = HashMap::new();
    {
        let c = h1_closes.entry(code.clone()).or_default();
        let h = h1_highs.entry(code.clone()).or_default();
        let l = h1_lows.entry(code.clone()).or_default();
        for i in 0..40u64 {
            let osc = if i % 2 == 0 { 50_000 } else { 0 };
            let base = Decimal::from(10_000_000u64 + osc);
            c.push(base);
            h.push(base + dec!(3000));
            l.push(base - dec!(3000));
        }
    }
    let h1_candle = auto_trader_core::types::Candle {
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

    let (mut h1_event, _h1_ind) =
        emit_candle_event(h1_candle, &mut h1_closes, &mut h1_highs, &mut h1_lows, true);
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

#[test]
fn h1_builder_does_not_emit_mid_period() {
    let pair = Pair::new("FX_BTC_JPY");
    let mut h1 = CandleBuilder::new(pair, Exchange::BitflyerCfd, "H1".to_string());
    let base = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();

    assert!(
        h1.on_tick(dec!(10_000_000), dec!(0.1), base, None, None)
            .is_none(),
        "first tick in a new H1 period must not emit a candle"
    );
    assert!(
        h1.on_tick(
            dec!(10_010_000),
            dec!(0.2),
            base + chrono::Duration::minutes(30),
            None,
            None
        )
        .is_none(),
        "a second tick within the same H1 period must not emit a candle"
    );
    assert!(
        h1.try_complete(base + chrono::Duration::minutes(59), None, None)
            .is_none(),
        "try_complete must not emit before the H1 period boundary"
    );
}

#[test]
fn h1_builder_emits_on_period_boundary() {
    let pair = Pair::new("FX_BTC_JPY");
    let mut h1 = CandleBuilder::new(pair, Exchange::BitflyerCfd, "H1".to_string());
    let base = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();

    h1.on_tick(dec!(10_000_000), dec!(0.1), base, None, None);

    let candle = h1.on_tick(
        dec!(10_100_000),
        dec!(0.1),
        base + chrono::Duration::hours(1),
        None,
        None,
    );
    assert!(
        candle.is_some(),
        "H1 candle should be emitted at period boundary"
    );
    let c = candle.unwrap();
    assert_eq!(c.timeframe, "H1");
    assert_eq!(c.open, dec!(10_000_000));
    assert_eq!(c.close, dec!(10_000_000));
}
