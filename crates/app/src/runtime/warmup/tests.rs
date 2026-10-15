use auto_trader_core::types::Pair;
use chrono::{TimeZone, Utc};
use rust_decimal_macros::dec;

use super::*;

fn make_candle(pair: &str, timestamp: chrono::DateTime<Utc>) -> Candle {
    Candle {
        pair: Pair::new(pair),
        exchange: Exchange::BitflyerCfd,
        timeframe: "1m".to_string(),
        open: dec!(100),
        high: dec!(110),
        low: dec!(90),
        close: dec!(105),
        volume: Some(10),
        best_bid: Some(dec!(104)),
        best_ask: Some(dec!(106)),
        timestamp,
    }
}

#[test]
fn to_price_events_maps_fields_and_sets_empty_indicators() {
    let ts = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let candle = make_candle("FX_BTC_JPY", ts);

    let events = to_price_events(vec![candle.clone()], Exchange::GmoFx);

    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.pair, candle.pair);
    assert_eq!(event.exchange, Exchange::GmoFx);
    assert_eq!(event.timestamp, ts);
    assert_eq!(event.candle.close, candle.close);
    assert!(event.indicators.is_empty());
}

#[test]
fn to_price_events_returns_empty_vec_for_empty_input() {
    let events = to_price_events(Vec::new(), Exchange::Oanda);

    assert!(events.is_empty());
}

#[test]
fn to_price_events_preserves_input_order() {
    let ts = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let first = make_candle("FX_BTC_JPY", ts);
    let second = make_candle("ETH_JPY", ts);

    let events = to_price_events(vec![first.clone(), second.clone()], Exchange::BitflyerCfd);

    assert_eq!(events[0].pair, first.pair);
    assert_eq!(events[1].pair, second.pair);
}
