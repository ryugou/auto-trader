use auto_trader_core::types::Pair;

use super::*;
use crate::test_support::{
    make_event, make_position_with_sl, warmup_with_drift, warmup_with_flat_candles,
};

fn default_params() -> serde_json::Value {
    serde_json::json!({
        "entry_channel": 20,
        "exit_channel": 10,
        "atr_baseline_bars": 50
    })
}

#[test]
fn constructor_parses_json_params() {
    let params = serde_json::json!({
        "entry_channel": 18,
        "exit_channel": 8,
        "atr_baseline_bars": 30
    });
    let s = DonchianTrendEvolveV1::new("test".to_string(), vec![Pair::new("FX_BTC_JPY")], params);
    assert_eq!(s.entry_channel, 18);
    assert_eq!(s.exit_channel, 8);
    assert_eq!(s.atr_baseline_bars, 30);
}

#[test]
fn constructor_uses_defaults_for_missing_keys() {
    let s = DonchianTrendEvolveV1::new(
        "test".to_string(),
        vec![Pair::new("FX_BTC_JPY")],
        serde_json::json!({}),
    );
    assert_eq!(s.entry_channel, 20);
    assert_eq!(s.exit_channel, 10);
    assert_eq!(s.atr_baseline_bars, 20);
}

#[test]
fn constructor_ignores_legacy_sl_allocation_params() {
    let params = serde_json::json!({
        "entry_channel": 20,
        "exit_channel": 10,
        "sl_pct": 0.04,
        "allocation_pct": 0.8,
        "atr_baseline_bars": 50
    });
    let s = DonchianTrendEvolveV1::new("test".to_string(), vec![Pair::new("FX_BTC_JPY")], params);
    assert_eq!(s.entry_channel, 20);
    assert_eq!(s.exit_channel, 10);
    assert_eq!(s.atr_baseline_bars, 50);
}

type ClampCase = (
    &'static str,
    u64,
    usize,
    fn(&DonchianTrendEvolveV1) -> usize,
);

#[test]
fn constructor_clamps_params_to_allowed_range() {
    let cases: [ClampCase; 6] = [
        ("entry_channel", 1, 10, |s| s.entry_channel),
        ("entry_channel", 999, 30, |s| s.entry_channel),
        ("exit_channel", 1, 5, |s| s.exit_channel),
        ("exit_channel", 999, 15, |s| s.exit_channel),
        ("atr_baseline_bars", 1, 20, |s| s.atr_baseline_bars),
        ("atr_baseline_bars", 999, 100, |s| s.atr_baseline_bars),
    ];
    for (param, input, expected, field) in cases {
        let mut params = serde_json::Map::new();
        params.insert(param.to_string(), serde_json::json!(input));
        let s = DonchianTrendEvolveV1::new(
            "test".to_string(),
            vec![Pair::new("FX_BTC_JPY")],
            serde_json::Value::Object(params),
        );
        assert_eq!(field(&s), expected, "param {param} with input {input}");
    }
}

#[tokio::test]
async fn no_signal_with_insufficient_history() {
    let mut s = DonchianTrendEvolveV1::new(
        "dte".to_string(),
        vec![Pair::new("FX_BTC_JPY")],
        default_params(),
    );
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
async fn long_breakout_with_atr_based_sl() {
    let params = serde_json::json!({
        "entry_channel": 20,
        "exit_channel": 10,
        "atr_baseline_bars": 50
    });
    let mut s =
        DonchianTrendEvolveV1::new("dte".to_string(), vec![Pair::new("FX_BTC_JPY")], params);
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
    let signal = s.on_price(&breakout).await;
    assert!(signal.is_some(), "expected long breakout signal");
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert!(
        sig.stop_loss_pct > Decimal::ZERO && sig.stop_loss_pct <= dec!(0.05),
        "ATR-based SL must be in (0, SL_CAP=5%], got {}",
        sig.stop_loss_pct
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
async fn position_closes_on_trailing_channel() {
    let mut s = DonchianTrendEvolveV1::new(
        "dte".to_string(),
        vec![Pair::new("FX_BTC_JPY")],
        default_params(),
    );
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
        "dte",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(10000000),
        dec!(9800000),
    );
    let drop_event = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(10500000),
        dec!(10550000),
        dec!(10450000),
    );
    let _ = s.on_price(&drop_event).await;
    let exits = s
        .on_open_positions(std::slice::from_ref(&pos), &drop_event)
        .await;
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);
}

#[tokio::test]
async fn position_short_exits_on_trailing_after_1r() {
    let mut s = DonchianTrendEvolveV1::new(
        "dte".to_string(),
        vec![Pair::new("FX_BTC_JPY")],
        default_params(),
    );
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
        "dte",
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
async fn position_no_exit_when_1r_not_reached() {
    let mut s = DonchianTrendEvolveV1::new(
        "dte".to_string(),
        vec![Pair::new("FX_BTC_JPY")],
        default_params(),
    );
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
        "dte",
        "FX_BTC_JPY",
        Direction::Long,
        dec!(10800000),
        dec!(10600000),
    );
    let drop_event = make_event(
        "FX_BTC_JPY",
        "H1",
        dec!(10900000),
        dec!(10950000),
        dec!(10850000),
    );
    let _ = s.on_price(&drop_event).await;
    let exits = s
        .on_open_positions(std::slice::from_ref(&pos), &drop_event)
        .await;
    assert!(
        exits.is_empty(),
        "1R not reached → no trailing exit, got {} exits",
        exits.len()
    );
}
