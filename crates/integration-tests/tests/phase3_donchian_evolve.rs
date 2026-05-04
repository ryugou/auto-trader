//! Phase 3A: Donchian Trend Evolve V1 strategy tests.
//!
//! DonchianTrendEvolveV1 is a parameterizable version of DonchianTrendV1
//! that reads channel parameters from a JSON blob (strategy_params DB table).
//! These tests verify constructor behavior: custom params, default fallback,
//! and invalid-params clamping.
//!
//! Requirements: 3.26 (custom params), 3.27 (default fallback), 3.28 (invalid → clamp).

use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{Direction, Exchange, Pair};
use auto_trader_integration_tests::helpers::trade_flow::{fixtures_dir, load_events_from_csv};
use auto_trader_strategy::donchian_trend_evolve::DonchianTrendEvolveV1;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const PAIR: &str = "USD_JPY";
const TIMEFRAME: &str = "H1";

// ─── 3.26: Custom parameters from DB ────────────────────────────────────

/// Custom params (entry_channel=15, exit_channel=8, atr_baseline_bars=25)
/// should be parsed correctly and the strategy should still produce signals
/// with the same CSV fixture data (breakout logic is the same, just the
/// channel widths change).
/// Minimum history = 15 + 25 + 14 + 1 = 55. CSV warmup = 61 bars → OK.
#[tokio::test]
async fn donchian_evolve_custom_params() {
    let params = serde_json::json!({
        "entry_channel": 15,
        "exit_channel": 8,
        "atr_baseline_bars": 25
    });
    let mut strategy = DonchianTrendEvolveV1::new(
        "donchian_trend_evolve_v1".to_string(),
        vec![Pair::new(PAIR)],
        params,
    );

    // Use the donchian_long_breakout fixture. With entry_channel=15
    // (narrower than the baseline 20), the breakout bar at 152.000 still
    // exceeds the 15-bar channel high (≈ 150.060 from near-flat data).
    // close=152.000 > 150.060 → breakout fires.
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_long_breakout.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Long breakout signal with custom params (entry_channel=15)"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert_eq!(sig.strategy_name, "donchian_trend_evolve_v1");
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.05));
    assert!(sig.take_profit_pct.is_none());
}

// ─── 3.27: Default fallback ─────────────────────────────────────────────

/// When the params JSON is empty `{}`, the constructor falls back to
/// baseline defaults (entry_channel=20, exit_channel=10, atr_baseline_bars=20).
/// The strategy should behave identically to the baseline DonchianTrendV1.
#[tokio::test]
async fn donchian_evolve_default_fallback() {
    let mut strategy = DonchianTrendEvolveV1::new(
        "donchian_trend_evolve_v1".to_string(),
        vec![Pair::new(PAIR)],
        serde_json::json!({}),
    );

    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_long_breakout.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Long breakout signal with default fallback params"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    // Default behavior: same as baseline
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.05));
    assert!(sig.take_profit_pct.is_none());
}

// ─── 3.28: Invalid parameters → clamp ───────────────────────────────────

/// Out-of-range params should be clamped to safe ranges:
/// - entry_channel: clamp(5, 10, 30) → 10
/// - exit_channel: clamp(1, 5, 15) → 5
/// - atr_baseline_bars: clamp(10, 20, 100) → 20
/// Minimum history = 10 + 20 + 14 + 1 = 45. CSV warmup = 61 bars → OK.
/// The strategy should still function correctly after clamping.
#[tokio::test]
async fn donchian_evolve_invalid_params_clamp() {
    let params = serde_json::json!({
        "entry_channel": 5,       // min=10 → clamped to 10
        "exit_channel": 1,        // min=5 → clamped to 5
        "atr_baseline_bars": 10   // min=20 → clamped to 20
    });
    let mut strategy = DonchianTrendEvolveV1::new(
        "donchian_trend_evolve_v1".to_string(),
        vec![Pair::new(PAIR)],
        params,
    );

    // With entry_channel=10 (clamped from 5), the channel is narrower.
    // The breakout bar at close=152.000 should still exceed the 10-bar high
    // (≈ 150.060 from the flat fixture data).
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_long_breakout.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Long breakout signal with clamped params (entry_channel=10)"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.05));
}
