use auto_trader_core::types::Pair;

use super::*;
use crate::test_support::{
    make_event, make_position_with_sl, warmup_with_drift, warmup_with_flat_candles,
};

#[tokio::test]
async fn no_signal_with_insufficient_history() {
    let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let e = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(10000000),
        dec!(10005000),
        dec!(9995000),
    );
    assert!(s.on_price(&e).await.is_none());
}

#[tokio::test]
async fn ignores_non_h1_timeframe() {
    let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let mut e = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(10000000),
        dec!(10005000),
        dec!(9995000),
    );
    e.candle.timeframe = "M5".to_string();
    for _ in 0..120 {
        assert!(s.on_price(&e).await.is_none());
    }
}

#[tokio::test]
async fn long_breakout_above_channel_with_volatility_expansion() {
    let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_drift(
        &mut s,
        "FX_BTC_JPY",
        "H1",
        dec!(10000000),
        dec!(10000),
        100,
        |i| Decimal::from(i % 5) * dec!(1000),
    )
    .await;
    let close_above_prior_high = dec!(11000000);
    let high_wide_enough_to_lift_atr_above_baseline = dec!(11200000);
    let low_wide_enough_to_lift_atr_above_baseline = dec!(10800000);
    let breakout = make_event(
        "FX_BTC_JPY",
        "H1",
        close_above_prior_high,
        high_wide_enough_to_lift_atr_above_baseline,
        low_wide_enough_to_lift_atr_above_baseline,
    );
    let signal = s.on_price(&breakout).await;
    assert!(signal.is_some(), "expected long breakout signal");
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert!(
        sig.stop_loss_pct > Decimal::ZERO,
        "ATR-based SL must be positive"
    );
    assert!(
        sig.stop_loss_pct <= dec!(0.05),
        "ATR-based SL must not exceed SL_CAP"
    );
    assert!(
        sig.allocation_pct > Decimal::ZERO,
        "allocation must be positive"
    );
    assert!(
        sig.allocation_pct <= dec!(1.00),
        "allocation must not exceed ALLOCATION_CAP"
    );
    assert!(
        sig.take_profit_pct.is_none(),
        "Turtle breakout has no fixed take profit"
    );
}

#[tokio::test]
async fn sl_and_allocation_are_atr_derived() {
    let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_drift(
        &mut s,
        "FX_BTC_JPY",
        "H1",
        dec!(10000000),
        dec!(10000),
        100,
        |i| Decimal::from(i % 5) * dec!(1000),
    )
    .await;
    let breakout = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(11000000),
        dec!(11200000),
        dec!(10800000),
    );
    let sig = s.on_price(&breakout).await.unwrap();
    assert!(
        sig.stop_loss_pct > Decimal::ZERO && sig.stop_loss_pct <= dec!(0.05),
        "ATR-based SL must be in (0, SL_CAP=5%], got {}",
        sig.stop_loss_pct
    );
    assert!(
        sig.allocation_pct > Decimal::ZERO && sig.allocation_pct <= dec!(1.00),
        "allocation must be in (0, 100%], got {}",
        sig.allocation_pct
    );
}

#[tokio::test]
async fn open_positions_close_on_trailing_channel_break() {
    let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_flat_candles(
        &mut s,
        "FX_BTC_JPY",
        "H1",
        dec!(11000000),
        dec!(11050000),
        dec!(10950000),
        100,
    )
    .await;
    let pos = make_position_with_sl(
        "dt",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(10000000),
        dec!(9800000),
    );
    let drop = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(10500000),
        dec!(10550000),
        dec!(10450000),
    );
    let _ = s.on_price(&drop).await;
    let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
    assert_eq!(exits.len(), 1, "expected trailing channel exit");
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);
    assert_eq!(exits[0].close_price, dec!(10500000));
}

#[tokio::test]
async fn open_positions_short_exits_on_trailing_after_1r() {
    let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_flat_candles(
        &mut s,
        "FX_BTC_JPY",
        "H1",
        dec!(11000000),
        dec!(11050000),
        dec!(10950000),
        100,
    )
    .await;
    let pos = make_position_with_sl(
        "dt",
        "FX_BTC_JPY",
        Direction::Short,
        dec!(11300000),
        dec!(11500000),
    );
    let spike = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(11100000),
        dec!(11150000),
        dec!(11050000),
    );
    let _ = s.on_price(&spike).await;
    let exits = s
        .on_open_positions(std::slice::from_ref(&pos), &spike)
        .await;
    assert_eq!(exits.len(), 1, "expected trailing channel exit for Short");
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);
    assert_eq!(exits[0].close_price, dec!(11100000));
}

#[tokio::test]
async fn open_positions_no_exit_when_1r_not_reached() {
    let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_flat_candles(
        &mut s,
        "FX_BTC_JPY",
        "H1",
        dec!(11000000),
        dec!(11050000),
        dec!(10950000),
        100,
    )
    .await;
    let pos = make_position_with_sl(
        "dt",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(10800000),
        dec!(10600000),
    );
    let drop = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(10900000),
        dec!(10950000),
        dec!(10850000),
    );
    let _ = s.on_price(&drop).await;
    let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
    assert!(
        exits.is_empty(),
        "1R not reached → no trailing exit, got {} exits",
        exits.len()
    );
}
