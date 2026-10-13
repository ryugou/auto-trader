use auto_trader_core::types::Pair;

use super::*;
use crate::test_support::{
    make_event, make_position_with_sl, warmup_with_drift, warmup_with_flat_candles,
};

#[tokio::test]
async fn no_signal_until_history_warmed() {
    let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let e = make_event(
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(10010000),
        dec!(9990000),
    );
    assert!(s.on_price(&e).await.is_none());
}

#[tokio::test]
async fn ignores_non_m5_timeframe() {
    let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    let mut e = make_event(
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(10010000),
        dec!(9990000),
    );
    e.candle.timeframe = "H1".to_string();
    for _ in 0..50 {
        assert!(
            s.on_price(&e).await.is_none(),
            "H1 candle must not trigger a signal even after many bars"
        );
    }
}

#[tokio::test]
async fn long_signal_at_oversold_extreme_with_capitulation() {
    let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_drift(
        &mut s,
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(5000),
        30,
        |i| Decimal::from(i * 100),
    )
    .await;
    let capitulation_low = dec!(8990000);
    let crash = make_event(
        "FX_BTC_JPY",
        "M5",
        dec!(9000000),
        dec!(9050000),
        capitulation_low,
    );
    let signal = s.on_price(&crash).await;
    assert!(
        signal.is_some(),
        "expected long mean-revert signal at extreme"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert!(
        sig.stop_loss_pct > Decimal::ZERO,
        "ATR-based SL must be positive"
    );
    assert!(
        sig.stop_loss_pct <= dec!(0.03),
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
        "take profit must be None for the dynamic mean-reversion exit"
    );
    assert!(
        sig.max_hold_until.is_some(),
        "the 24h fail-safe must set max_hold_until"
    );
}

#[tokio::test]
async fn sl_and_allocation_are_atr_derived() {
    let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_drift(
        &mut s,
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(5000),
        30,
        |i| Decimal::from(i * 100),
    )
    .await;
    let crash = make_event(
        "FX_BTC_JPY",
        "M5",
        dec!(9000000),
        dec!(9050000),
        dec!(8990000),
    );
    let sig = s.on_price(&crash).await.unwrap();
    assert!(
        sig.stop_loss_pct > Decimal::ZERO && sig.stop_loss_pct <= dec!(0.03),
        "ATR-based SL must be in (0, SL_CAP=3%], got {}",
        sig.stop_loss_pct
    );
    assert!(
        sig.allocation_pct > Decimal::ZERO && sig.allocation_pct <= dec!(1.00),
        "allocation must be in (0, 100%], got {}",
        sig.allocation_pct
    );
}

#[tokio::test]
async fn open_positions_close_at_mean() {
    let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_flat_candles(
        &mut s,
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(10005000),
        dec!(9995000),
        30,
    )
    .await;
    let pos = make_position_with_sl(
        "bb",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(9500000),
        dec!(9300000),
    );
    let event = make_event(
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(10005000),
        dec!(9995000),
    );
    let exits = s
        .on_open_positions(std::slice::from_ref(&pos), &event)
        .await;
    assert_eq!(exits.len(), 1, "expected 1 mean-reached exit");
    assert_eq!(exits[0].trade_id, pos.trade.id);
    assert_eq!(exits[0].reason, StrategyExitReason::MeanReached);
    assert_eq!(exits[0].close_price, dec!(10000000));
}

#[tokio::test]
async fn open_positions_no_exit_when_1r_not_reached() {
    let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_flat_candles(
        &mut s,
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(10005000),
        dec!(9995000),
        30,
    )
    .await;
    let pos = make_position_with_sl(
        "bb",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(9900000),
        dec!(9700000),
    );
    let event = make_event(
        "FX_BTC_JPY",
        "M5",
        dec!(10050000),
        dec!(10060000),
        dec!(10040000),
    );
    let exits = s
        .on_open_positions(std::slice::from_ref(&pos), &event)
        .await;
    assert!(
        exits.is_empty(),
        "1R not reached → no exit, got {} exits",
        exits.len()
    );
}

#[tokio::test]
async fn open_positions_short_exits_at_mean_after_1r() {
    let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_flat_candles(
        &mut s,
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(10005000),
        dec!(9995000),
        30,
    )
    .await;
    let pos = make_position_with_sl(
        "bb",
        "FX_BTC_JPY",
        Direction::Short,
        dec!(10300000),
        dec!(10500000),
    );
    let event = make_event(
        "FX_BTC_JPY",
        "M5",
        dec!(9900000),
        dec!(9950000),
        dec!(9850000),
    );
    let exits = s
        .on_open_positions(std::slice::from_ref(&pos), &event)
        .await;
    assert_eq!(exits.len(), 1, "expected 1 mean-reached exit for Short");
    assert_eq!(exits[0].trade_id, pos.trade.id);
    assert_eq!(exits[0].reason, StrategyExitReason::MeanReached);
    assert_eq!(exits[0].close_price, dec!(9900000));
}

#[tokio::test]
async fn open_positions_ignore_other_strategies() {
    let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
    warmup_with_flat_candles(
        &mut s,
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(10005000),
        dec!(9995000),
        30,
    )
    .await;
    let pos = make_position_with_sl(
        "not_bb",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(9500000),
        dec!(9400000),
    );
    let event = make_event(
        "FX_BTC_JPY",
        "M5",
        dec!(10000000),
        dec!(10005000),
        dec!(9995000),
    );
    let exits = s.on_open_positions(&[pos], &event).await;
    assert!(exits.is_empty());
}
