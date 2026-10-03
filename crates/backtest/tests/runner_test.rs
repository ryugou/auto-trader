//! Integration tests for the modernized backtest runner.
//!
//! Seeds synthetic H1 candles for bitFlyer Crypto CFD (FX_BTC_JPY) in a
//! flat → breakout → crash shape, replays `donchian_trend_v1`, and verifies:
//!   (a) an entry actually occurs, and
//!   (b) the SL close's PnL is quantity-based — a concrete regression guard
//!       against the old `price_diff × leverage` bug.

use auto_trader_backtest::runner::BacktestRunner;
use auto_trader_core::types::{Candle, Exchange, ExitReason, Pair, TradeStatus};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_strategy::donchian_trend::DonchianTrendV1;
use chrono::{Duration, TimeZone, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;
use std::collections::HashMap;

const EXCHANGE: Exchange = Exchange::BitflyerCfd;
const PAIR: &str = "FX_BTC_JPY";
const TIMEFRAME: &str = "H1";

fn candle(ts_index: i64, open: Decimal, high: Decimal, low: Decimal, close: Decimal) -> Candle {
    let base = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
    Candle {
        pair: Pair::new(PAIR),
        exchange: EXCHANGE,
        timeframe: TIMEFRAME.to_string(),
        open,
        high,
        low,
        close,
        volume: Some(1),
        best_bid: None,
        best_ask: None,
        timestamp: base + Duration::hours(ts_index),
    }
}

/// flat low-vol phase → breakout bar (Long entry) → crash bar (SL hit).
fn synthetic_candles() -> Vec<Candle> {
    let mut candles = Vec::new();
    let mut idx: i64 = 0;

    // 60 flat bars: constant close, small ±2000 range. channel_high stays at
    // 10_002_000 so the flat closes (10_000_000) never break out, and ATR ≈
    // baseline so the volatility filter also blocks entries here.
    for _ in 0..60 {
        candles.push(candle(
            idx,
            dec!(10_000_000),
            dec!(10_002_000),
            dec!(9_998_000),
            dec!(10_000_000),
        ));
        idx += 1;
    }

    // Breakout bar: close far above the 20-bar channel high AND a large true
    // range so ATR(14) >> baseline → donchian_trend_v1 emits a Long entry.
    candles.push(candle(
        idx,
        dec!(10_000_000),
        dec!(10_520_000),
        dec!(10_000_000),
        dec!(10_500_000),
    ));
    idx += 1;

    // Crash bar: low collapses well below the trade's stop-loss → SL hit.
    candles.push(candle(
        idx,
        dec!(10_500_000),
        dec!(10_500_000),
        dec!(9_800_000),
        dec!(10_000_000),
    ));

    candles
}

fn btc_sizer() -> PositionSizer {
    let mut min_sizes = HashMap::new();
    // bitFlyer FX_BTC_JPY minimum order size.
    min_sizes.insert(Pair::new(PAIR), dec!(0.001));
    // Backtest uses margin_buffer = 0 so sizing matches the raw no-liquidation
    // cap (documented choice — the buffer is a live safety margin).
    PositionSizer::new(min_sizes, Decimal::ZERO)
}

#[sqlx::test(migrations = "../../migrations")]
async fn donchian_entry_and_qty_based_sl_pnl(pool: sqlx::PgPool) {
    // Seed synthetic candles.
    for c in synthetic_candles() {
        auto_trader_db::candles::upsert_candle(&pool, &c)
            .await
            .expect("seed candle");
    }

    let mut strategy = DonchianTrendV1::new("donchian_trend_v1".to_string(), vec![Pair::new(PAIR)]);
    let runner = BacktestRunner::new(pool);

    let initial_balance = dec!(300_000);
    let leverage = dec!(4);
    let liquidation_margin_level = dec!(0.5); // bitFlyer Crypto CFD Y = 50%
    let spread_pct = dec!(0.0001); // 0.01% flat approximation

    let report = runner
        .run(
            &mut strategy,
            EXCHANGE,
            &Pair::new(PAIR),
            TIMEFRAME,
            initial_balance,
            leverage,
            &btc_sizer(),
            liquidation_margin_level,
            spread_pct,
        )
        .await
        .expect("backtest run");

    // (a) an entry occurred and closed via SL.
    assert!(
        report.total_trades >= 1,
        "expected at least one closed trade, got {}",
        report.total_trades
    );
    let sl_trade = report
        .trades
        .iter()
        .find(|t| t.status == TradeStatus::Closed && t.exit_reason == Some(ExitReason::SlHit))
        .expect("an SL-closed trade must exist");

    // (b) PnL is quantity-based: pnl == truncate_toward_zero((exit - entry) × qty).
    let entry = sl_trade.entry_price;
    let exit = sl_trade.exit_price.expect("closed trade has exit price");
    let qty = sl_trade.quantity;
    assert!(qty > Decimal::ZERO, "quantity must be sized > 0, got {qty}");
    assert_ne!(
        qty,
        Decimal::ONE,
        "quantity must be sized, not the ONE placeholder"
    );

    let expected_pnl = ((exit - entry) * qty).round_dp_with_strategy(0, RoundingStrategy::ToZero);
    assert_eq!(
        sl_trade.pnl_amount,
        Some(expected_pnl),
        "pnl must equal (exit - entry) × quantity truncated toward zero"
    );

    // Regression guard: the old bug computed price_diff × leverage (ignoring
    // quantity). Ensure we are NOT accidentally reproducing that value.
    let buggy_pnl = (exit - entry) * leverage;
    assert_ne!(
        sl_trade.pnl_amount,
        Some(buggy_pnl),
        "pnl must not be the old price_diff × leverage value"
    );
}
