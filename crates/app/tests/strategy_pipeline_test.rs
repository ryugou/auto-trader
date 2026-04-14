//! Strategy → PositionSizer pipeline integration tests.
//!
//! These tests catch the class of bug where a strategy emits a perfectly
//! valid Signal but the sizer rejects it. Strategies and sizer are each
//! unit-tested in isolation, but both layers' tests passed while the
//! strategies could not actually open positions on the production paper
//! accounts. The fix decoupled the layers entirely:
//!
//! - Signal layer = chart only (price levels, allocation_pct).
//! - Execution layer = balance only (sizer ignores everything chart).
//!
//! Each test uses the same account profile as the live paper accounts:
//!   balance       = 30,000 JPY
//!   leverage      = 2x
//!   min_lot       = 0.001 BTC
//!   pair          = FX_BTC_JPY
//!
//! and walks the strategy through enough warmup candles to reach a
//! realistic indicator state, then verifies that *whenever the strategy
//! emits a signal, the sizer accepts it*.

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{Candle, Exchange, Pair};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_strategy::bb_mean_revert::BbMeanRevertV1;
use auto_trader_strategy::donchian_trend::DonchianTrendV1;
use auto_trader_strategy::squeeze_momentum::SqueezeMomentumV1;
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;

const PAIR: &str = "FX_BTC_JPY";
const ACCOUNT_BALANCE: Decimal = dec!(30000);
const LEVERAGE: Decimal = dec!(2);
const MIN_LOT: Decimal = dec!(0.001);

fn live_account_sizer() -> PositionSizer {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new(PAIR), MIN_LOT);
    PositionSizer::new(min_sizes)
}

fn make_event(close: Decimal, high: Decimal, low: Decimal, idx: i64) -> PriceEvent {
    let ts = Utc.timestamp_opt(1_700_000_000 + idx * 300, 0).unwrap();
    PriceEvent {
        pair: Pair::new(PAIR),
        exchange: Exchange::BitflyerCfd,
        timestamp: ts,
        candle: Candle {
            pair: Pair::new(PAIR),
            exchange: Exchange::BitflyerCfd,
            timeframe: "M5".to_string(),
            open: close,
            high,
            low,
            close,
            volume: Some(0),
            timestamp: ts,
        },
        indicators: HashMap::new(),
    }
}

/// Build a flat-then-trend candle series tuned to give Donchian /
/// squeeze strategies a clear breakout. Returns the slice up to (but
/// not including) the breakout candle so callers can warm the strategy
/// first.
fn flat_then_trend(
    base: Decimal,
    flat_bars: usize,
    trend_step: Decimal,
    trend_bars: usize,
) -> Vec<PriceEvent> {
    let mut out = Vec::with_capacity(flat_bars + trend_bars);
    for i in 0..flat_bars {
        // Tiny zig-zag so ATR isn't literally zero.
        let drift = if i % 2 == 0 { dec!(1000) } else { dec!(-1000) };
        let close = base + drift;
        out.push(make_event(
            close,
            close + dec!(2000),
            close - dec!(2000),
            i as i64,
        ));
    }
    for i in 0..trend_bars {
        let close = base + trend_step * Decimal::from((i + 1) as u64);
        out.push(make_event(
            close,
            close + dec!(3000),
            close - dec!(3000),
            (flat_bars + i) as i64,
        ));
    }
    out
}

/// **Regression test for the production bug** where donchian_trend_v1
/// could not place trades on the 30k JPY paper account because the
/// old risk-based sizer rejected the strategy's signal. The fix moved
/// to pure capacity sizing — this test verifies the strategy's signal
/// (now with `entry × 3%` flat SL and `allocation_pct = 0.6`) sizes
/// to a non-zero quantity on the live account profile.
#[tokio::test]
async fn donchian_signal_passes_sizer_on_30k_account() {
    let mut strat = DonchianTrendV1::new("donchian_trend_v1".to_string(), vec![Pair::new(PAIR)]);
    let sizer = live_account_sizer();

    // Warm up with 100 flat bars around 11M, then push a clear upside
    // breakout sequence so the channel + ATR filter both pass.
    let warm = flat_then_trend(dec!(11000000), 100, dec!(20000), 30);
    let mut emitted = None;
    for event in warm {
        if let Some(sig) = strat.on_price(&event).await {
            emitted = Some(sig);
            break;
        }
    }

    let signal =
        emitted.expect("donchian_trend_v1 must emit at least one entry signal in this trend setup");

    let qty = sizer.calculate_quantity(
        &signal.pair,
        ACCOUNT_BALANCE,
        signal.entry_price,
        LEVERAGE,
        signal.allocation_pct,
    );
    assert!(
        qty.is_some(),
        "donchian signal must size to >0 on a 30k JPY account (entry={}, allocation_pct={})",
        signal.entry_price,
        signal.allocation_pct
    );
    let qty = qty.unwrap();
    assert!(qty >= MIN_LOT, "quantity {qty} must be at least {MIN_LOT}");
}

/// Same regression check for squeeze_momentum_v1: with flat 4% SL and
/// `allocation_pct = 0.9`, the squeeze release signal must size to >0
/// on the live 30k JPY account.
#[tokio::test]
async fn squeeze_signal_passes_sizer_on_30k_account() {
    let mut strat =
        SqueezeMomentumV1::new("squeeze_momentum_v1".to_string(), vec![Pair::new(PAIR)]);
    let sizer = live_account_sizer();

    // 80 ultra-flat bars to force BB inside KC (build a sustained
    // squeeze counter), then a clear upside expansion.
    let mut events: Vec<PriceEvent> = (0..80)
        .map(|i| make_event(dec!(11000000), dec!(11000100), dec!(10999900), i))
        .collect();
    // Big up-bar that releases the squeeze with positive momentum.
    events.push(make_event(
        dec!(11500000),
        dec!(11600000),
        dec!(11000000),
        80,
    ));

    let mut emitted = None;
    for event in &events {
        if let Some(sig) = strat.on_price(event).await {
            emitted = Some(sig);
            break;
        }
    }

    let signal = emitted
        .expect("squeeze_momentum_v1 must emit a signal after a squeeze release with momentum");
    let qty = sizer.calculate_quantity(
        &signal.pair,
        ACCOUNT_BALANCE,
        signal.entry_price,
        LEVERAGE,
        signal.allocation_pct,
    );
    assert!(
        qty.is_some(),
        "squeeze signal must size to >0 on a 30k JPY account (entry={}, allocation_pct={})",
        signal.entry_price,
        signal.allocation_pct
    );
}

/// Same regression check for bb_mean_revert_v1: with flat 2% SL and
/// `allocation_pct = 0.3`, the BB-extreme reversal must size to >0
/// on the live 30k JPY account.
#[tokio::test]
async fn bb_mean_revert_signal_passes_sizer_on_30k_account() {
    let mut strat = BbMeanRevertV1::new("bb_mean_revert_v1".to_string(), vec![Pair::new(PAIR)]);
    let sizer = live_account_sizer();

    // Warm up with 30 flat candles around 11M. Then a sharp drop that
    // breaks below BB(20, 2.5σ) lower with a fresh lower-low.
    let mut events: Vec<PriceEvent> = (0..30)
        .map(|i| make_event(dec!(11000000), dec!(11005000), dec!(10995000), i))
        .collect();
    // Crash bar: close well below recent lows + lower low than prev bar.
    events.push(make_event(
        dec!(10000000),
        dec!(10050000),
        dec!(9950000),
        30,
    ));

    let mut emitted = None;
    for event in &events {
        if let Some(sig) = strat.on_price(event).await {
            emitted = Some(sig);
            break;
        }
    }

    let signal = emitted.expect("bb_mean_revert_v1 must emit at extremes");
    let qty = sizer.calculate_quantity(
        &signal.pair,
        ACCOUNT_BALANCE,
        signal.entry_price,
        LEVERAGE,
        signal.allocation_pct,
    );
    assert!(
        qty.is_some(),
        "bb_mean_revert signal must size to >0 on a 30k JPY account (entry={}, allocation_pct={})",
        signal.entry_price,
        signal.allocation_pct
    );
}

/// Sanity guard: a zero balance / zero price must NOT cause the sizer
/// to panic. Returns None.
#[tokio::test]
async fn sizer_handles_degenerate_inputs_without_panic() {
    let sizer = live_account_sizer();
    let qty = sizer.calculate_quantity(
        &Pair::new(PAIR),
        dec!(0),
        dec!(11000000),
        LEVERAGE,
        dec!(0.5),
    );
    assert_eq!(qty, None);
}
