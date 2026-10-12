use chrono::TimeZone;
use rust_decimal_macros::dec;

use super::*;

#[test]
fn builds_candle_from_ticks() {
    let pair = Pair::new("FX_BTC_JPY");
    let mut builder = CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, "M1".to_string());
    let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();

    assert!(
        builder
            .on_tick(dec!(15000000), dec!(0.1), base, None, None)
            .is_none()
    );
    assert!(
        builder
            .on_tick(
                dec!(15100000),
                dec!(0.2),
                base + chrono::Duration::seconds(10),
                Some(dec!(15099000)),
                Some(dec!(15101000)),
            )
            .is_none()
    );
    assert!(
        builder
            .on_tick(
                dec!(14900000),
                dec!(0.15),
                base + chrono::Duration::seconds(30),
                None,
                None,
            )
            .is_none()
    );
    assert!(
        builder
            .on_tick(
                dec!(15050000),
                dec!(0.05),
                base + chrono::Duration::seconds(50),
                Some(dec!(15049000)),
                Some(dec!(15051000)),
            )
            .is_none()
    );

    assert!(
        builder
            .try_complete(base + chrono::Duration::seconds(50), None, None)
            .is_none()
    );

    let candle = builder
        .try_complete(base + chrono::Duration::seconds(61), None, None)
        .unwrap();
    assert_eq!(candle.open, dec!(15000000));
    assert_eq!(candle.high, dec!(15100000));
    assert_eq!(candle.low, dec!(14900000));
    assert_eq!(candle.close, dec!(15050000));
    assert_eq!(candle.exchange, Exchange::BitflyerCfd);
    assert_eq!(candle.best_bid, Some(dec!(15049000)));
    assert_eq!(candle.best_ask, Some(dec!(15051000)));
}

#[test]
fn empty_period_returns_none() {
    let pair = Pair::new("FX_BTC_JPY");
    let mut builder = CandleBuilder::new(pair, Exchange::BitflyerCfd, "M1".to_string());
    let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();
    assert!(
        builder
            .try_complete(base + chrono::Duration::seconds(61), None, None)
            .is_none()
    );
}

#[test]
fn period_boundary_completes_previous_candle() {
    let pair = Pair::new("FX_BTC_JPY");
    let mut builder = CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, "M1".to_string());
    let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();

    assert!(
        builder
            .on_tick(dec!(100), dec!(10), base, None, None)
            .is_none()
    );
    let candle = builder.on_tick(
        dec!(200),
        dec!(5),
        base + chrono::Duration::seconds(60),
        None,
        None,
    );
    assert!(
        candle.is_some(),
        "on_tick should return completed candle at period boundary"
    );
    let candle = candle.unwrap();
    assert_eq!(candle.open, dec!(100));
    assert_eq!(candle.close, dec!(100));
    assert_eq!(candle.best_bid, None);
    assert_eq!(candle.best_ask, None);

    let candle2 = builder
        .try_complete(base + chrono::Duration::seconds(121), None, None)
        .unwrap();
    assert_eq!(candle2.open, dec!(200));
    assert_eq!(candle2.close, dec!(200));
}

#[test]
fn on_tick_period_boundary_does_not_leak_new_period_bid_ask_into_completed_candle() {
    let pair = Pair::new("FX_BTC_JPY");
    let mut builder = CandleBuilder::new(pair, Exchange::BitflyerCfd, "M1".to_string());
    let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();

    assert!(
        builder
            .on_tick(dec!(100), dec!(1), base, Some(dec!(99)), Some(dec!(101)))
            .is_none()
    );

    let candle = builder
        .on_tick(
            dec!(200),
            dec!(1),
            base + chrono::Duration::seconds(60),
            Some(dec!(199)),
            Some(dec!(201)),
        )
        .expect("tick in next period should complete the previous candle");

    assert_eq!(candle.best_bid, Some(dec!(99)));
    assert_eq!(candle.best_ask, Some(dec!(101)));
}

#[test]
fn try_complete_returns_none_when_period_end_overflows() {
    let mut builder = CandleBuilder::new(
        Pair::new("FX_BTC_JPY"),
        Exchange::BitflyerCfd,
        "M1".to_string(),
    );
    let max = DateTime::<Utc>::MAX_UTC;
    assert!(builder.on_tick(dec!(1), dec!(1), max, None, None).is_none());

    assert!(builder.try_complete(max, None, None).is_none());
}
