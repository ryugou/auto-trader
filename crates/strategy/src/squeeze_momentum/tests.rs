use auto_trader_core::types::Pair;
use chrono::{DateTime, Utc};

use super::*;
use crate::test_support::{make_event, make_event_at, make_position_with_sl_at};

async fn warmup_hourly(
    s: &mut SqueezeMomentumV1,
    base_ts: DateTime<Utc>,
    step: Decimal,
    spread: Decimal,
) {
    for i in 0..50 {
        let ts = base_ts + Duration::hours(i);
        let p = dec!(10000000) + Decimal::from(i) * step;
        let _ = s
            .on_price(&make_event_at(
                "FX_BTC_JPY",
                "H1",
                p,
                p + spread,
                p - spread,
                ts,
            ))
            .await;
    }
}

#[test]
fn floor_to_hour_zeroes_minutes_seconds_and_nanos() {
    use chrono::TimeZone;
    let ts = Utc.with_ymd_and_hms(2026, 1, 1, 10, 45, 30).unwrap();
    let expected = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
    assert_eq!(SqueezeMomentumV1::floor_to_hour(ts), Some(expected));
}

#[tokio::test]
async fn no_signal_with_short_history() {
    let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let e = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(10000000),
        dec!(10010000),
        dec!(9990000),
    );
    assert!(s.on_price(&e).await.is_none());
}

#[tokio::test]
async fn ignores_non_h1_timeframe() {
    let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let mut e = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(10000000),
        dec!(10010000),
        dec!(9990000),
    );
    e.candle.timeframe = "M5".to_string();
    for _ in 0..100 {
        assert!(s.on_price(&e).await.is_none());
    }
}

#[tokio::test]
async fn fires_long_after_squeeze_release_with_momentum() {
    let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    for _ in 0..80 {
        let _ = s
            .on_price(&make_event(
                "FX_BTC_JPY",
                "H1",
                dec!(10000000),
                dec!(10000100),
                dec!(9999900),
            ))
            .await;
    }
    let breakout = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(10500000),
        dec!(10600000),
        dec!(10000000),
    );
    let signal = s.on_price(&breakout).await;
    assert!(signal.is_some(), "expected long squeeze-momentum signal");
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.05));
    assert!(
        sig.allocation_pct > Decimal::ZERO && sig.allocation_pct <= dec!(1.00),
        "allocation must be in (0, 100%], got {}",
        sig.allocation_pct
    );
    assert!(sig.take_profit_pct.is_none());
    assert!(sig.max_hold_until.is_some());
}

#[tokio::test]
async fn open_positions_close_on_chandelier_break() {
    let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let base_ts = Utc::now() - Duration::hours(60);

    warmup_hourly(&mut s, base_ts, dec!(10000), dec!(5000)).await;

    let entry_ts = base_ts + Duration::hours(10);
    let pos = make_position_with_sl_at(
        "sq",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(10100000),
        dec!(9900000),
        entry_ts,
    );

    let drop_ts = base_ts + Duration::hours(50);
    let drop = make_event_at(
        "FX_BTC_JPY",
        "H1",
        dec!(10350000),
        dec!(10380000),
        dec!(10320000),
        drop_ts,
    );
    let _ = s.on_price(&drop).await;
    let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
    assert_eq!(exits.len(), 1, "expected Chandelier trailing exit");
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingMa);
}

#[tokio::test]
async fn chandelier_does_not_exit_during_delay_phase() {
    let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let base_ts = Utc::now() - Duration::hours(60);

    warmup_hourly(&mut s, base_ts, dec!(10000), dec!(5000)).await;

    let entry_ts = base_ts + Duration::hours(49);
    let pos = make_position_with_sl_at(
        "sq",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(10490000),
        dec!(10300000),
        entry_ts,
    );

    let drop_ts = base_ts + Duration::hours(50);
    let drop = make_event_at(
        "FX_BTC_JPY",
        "H1",
        dec!(9000000),
        dec!(9050000),
        dec!(8950000),
        drop_ts,
    );
    let _ = s.on_price(&drop).await;
    let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
    assert_eq!(
        exits.len(),
        0,
        "should NOT exit during delay phase (first 3 bars)"
    );
}

#[tokio::test]
async fn chandelier_exits_at_boundary_of_delay_phase() {
    let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let base_ts = Utc::now() - Duration::hours(60);

    warmup_hourly(&mut s, base_ts, dec!(10000), dec!(5000)).await;

    let entry_ts = base_ts + Duration::hours(47);
    let pos = make_position_with_sl_at(
        "sq",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(10200000),
        dec!(10000000),
        entry_ts,
    );

    let drop_ts = base_ts + Duration::hours(50);
    let drop = make_event_at(
        "FX_BTC_JPY",
        "H1",
        dec!(10430000),
        dec!(10450000),
        dec!(10410000),
        drop_ts,
    );
    let _ = s.on_price(&drop).await;
    let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
    assert_eq!(
        exits.len(),
        1,
        "at bars_held == DELAY_BARS (boundary), trailing should be active"
    );
}

#[tokio::test]
async fn chandelier_no_exit_when_atr_is_zero() {
    let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let base_ts = Utc::now() - Duration::hours(60);
    let p = dec!(10000000);
    warmup_hourly(&mut s, base_ts, dec!(0), dec!(0)).await;
    let entry_ts = base_ts + Duration::hours(40);
    let pos = make_position_with_sl_at(
        "sq",
        "FX_BTC_JPY",
        Direction::Long,
        p,
        p - dec!(100000),
        entry_ts,
    );

    let flat_event = make_event_at("FX_BTC_JPY", "H1", p, p, p, base_ts + Duration::hours(50));
    let _ = s.on_price(&flat_event).await;
    let exits = s
        .on_open_positions(std::slice::from_ref(&pos), &flat_event)
        .await;
    assert_eq!(
        exits.len(),
        0,
        "ATR=0 should produce no chandelier exits, got {} exits",
        exits.len()
    );
}

#[tokio::test]
async fn chandelier_exits_short_on_break() {
    let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let base_ts = Utc::now() - Duration::hours(60);
    warmup_hourly(&mut s, base_ts, dec!(-10000), dec!(5000)).await;
    let entry_ts = base_ts + Duration::hours(10);
    let pos = make_position_with_sl_at(
        "sq",
        "FX_BTC_JPY",
        Direction::Short,
        dec!(9900000),
        dec!(10100000),
        entry_ts,
    );
    let spike_ts = base_ts + Duration::hours(50);
    let spike = make_event_at(
        "FX_BTC_JPY",
        "H1",
        dec!(9600000),
        dec!(9650000),
        dec!(9550000),
        spike_ts,
    );
    let _ = s.on_price(&spike).await;
    let exits = s
        .on_open_positions(std::slice::from_ref(&pos), &spike)
        .await;
    assert_eq!(
        exits.len(),
        1,
        "expected Short chandelier exit on upward break"
    );
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingMa);
}

#[tokio::test]
async fn chandelier_no_exit_when_1r_not_reached() {
    let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let base_ts = Utc::now() - Duration::hours(60);

    warmup_hourly(&mut s, base_ts, dec!(10000), dec!(5000)).await;

    let entry_ts = base_ts + Duration::hours(40);
    let pos = make_position_with_sl_at(
        "sq",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(10400000),
        dec!(10200000),
        entry_ts,
    );

    let drop_ts = base_ts + Duration::hours(50);
    let drop = make_event_at(
        "FX_BTC_JPY",
        "H1",
        dec!(10450000),
        dec!(10470000),
        dec!(10430000),
        drop_ts,
    );
    let _ = s.on_price(&drop).await;
    let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
    assert!(
        exits.is_empty(),
        "1R not reached → no Chandelier exit, got {} exits",
        exits.len()
    );
}
