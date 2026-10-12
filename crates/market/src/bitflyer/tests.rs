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
