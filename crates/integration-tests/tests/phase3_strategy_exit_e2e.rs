//! Phase 3 Task 6: Strategy exit reason → trader.close_position e2e tests.
//!
//! Each strategy emits `StrategyExitReason::*` from `on_open_positions` to
//! request a position close. The executor (`trader.close_position`) records
//! that reason as `ExitReason::*` on the trade row, computes pnl from the
//! exit fill price, and updates the account balance.
//!
//! These tests verify the full chain end-to-end:
//!
//!   strategy.on_open_positions → StrategyExitReason → ExitReason →
//!     trader.close_position → closed Trade { exit_reason, exit_price, pnl }.
//!
//! For the strategy-driven cases, we close via `exits[0].trade_id` (the id
//! the strategy actually returned) so a regression that mismatched the id
//! would surface here. The flagship `bb_mean_revert_long_*` test additionally
//! verifies the account ledger update (`current_balance` and `account_events`)
//! to confirm the close path runs through to the balance side. The other
//! tests intentionally focus on the trade-row side; ledger invariants for
//! every strategy/direction are covered by `phase3_pipeline_e2e.rs`.
//!
//! The `sl_hit_*` and `time_limit_*` tests verify only the executor's
//! handling of those `ExitReason`s. The position-monitor paths that decide
//! when to fire SL or time-limit closes are exercised separately in
//! `phase3_monitoring.rs` / `phase3_pipeline_e2e.rs`.
//!
//! Coverage:
//! - bb_mean_revert    MeanReached      Long / Short
//! - donchian_trend    TrailingChannel  Long / Short
//! - donchian_evolve   TrailingChannel  Long
//! - squeeze_momentum  TrailingMa       Long / Short
//! - manual time-limit StrategyTimeLimit (via max_hold_until)
//! - SL hit            SlHit            (price drops through SL)

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{Strategy, StrategyExitReason};
use auto_trader_core::types::{
    Candle, Direction, Exchange, ExitReason, Pair, Position, Signal, Trade, TradeStatus,
};
use auto_trader_integration_tests::helpers::pipeline::{PipelineHarness, PipelineHarnessConfig};
use auto_trader_integration_tests::helpers::trade_flow::{fixtures_dir, load_events_from_csv};
use auto_trader_strategy::bb_mean_revert::BbMeanRevertV1;
use auto_trader_strategy::donchian_trend::DonchianTrendV1;
use auto_trader_strategy::donchian_trend_evolve::DonchianTrendEvolveV1;
use auto_trader_strategy::squeeze_momentum::SqueezeMomentumV1;
use chrono::{DateTime, Duration, Timelike, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;
use sqlx::PgPool;
use std::collections::HashMap;

const PAIR: &str = "USD_JPY";
const EXCHANGE: Exchange = Exchange::GmoFx;
const SEED_BALANCE: i64 = 30_000;
const Y: Decimal = dec!(1.00); // GMO 外為 FX maintenance margin level (100%)
const MIN_LOT: Decimal = dec!(1);

// ─── Helpers ───────────────────────────────────────────────────────────────

/// Truncate-to-zero rounding to whole yen, mirroring `truncate_yen` in the trader.
fn truncate_yen(x: Decimal) -> Decimal {
    x.round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

/// Build a synthetic H1 PriceEvent (used to drive strategies past warmup).
fn make_h1_event(close: Decimal, high: Decimal, low: Decimal, ts: DateTime<Utc>) -> PriceEvent {
    PriceEvent {
        pair: Pair::new(PAIR),
        exchange: EXCHANGE,
        timestamp: ts,
        candle: Candle {
            pair: Pair::new(PAIR),
            exchange: EXCHANGE,
            timeframe: "H1".to_string(),
            open: close,
            high,
            low,
            close,
            volume: Some(0),
            best_bid: None,
            best_ask: None,
            timestamp: ts,
        },
        indicators: HashMap::new(),
    }
}

/// Build a synthetic M5 PriceEvent.
fn make_m5_event(close: Decimal, high: Decimal, low: Decimal, ts: DateTime<Utc>) -> PriceEvent {
    PriceEvent {
        pair: Pair::new(PAIR),
        exchange: EXCHANGE,
        timestamp: ts,
        candle: Candle {
            pair: Pair::new(PAIR),
            exchange: EXCHANGE,
            timeframe: "M5".to_string(),
            open: close,
            high,
            low,
            close,
            volume: Some(0),
            best_bid: None,
            best_ask: None,
            timestamp: ts,
        },
        indicators: HashMap::new(),
    }
}

/// Build a `PipelineHarness` configured for GmoFx + USD_JPY at 10x leverage —
/// the production-realistic FX setting also used by `phase3_pipeline_e2e.rs`.
async fn build_harness(pool: PgPool, account_label: &str) -> PipelineHarness {
    let mut harness = PipelineHarness::new(
        pool,
        PipelineHarnessConfig {
            account_name: account_label.to_string(),
            exchange: EXCHANGE,
            pair_str: PAIR.to_string(),
            strategy: "test_strategy".to_string(),
            balance: SEED_BALANCE,
            liquidation_margin_level: Y,
            min_order_size: MIN_LOT,
        },
    )
    .await;
    // Override seeded leverage (default 2) → 10x to mirror gmo_fx production.
    sqlx::query("UPDATE trading_accounts SET leverage = $1 WHERE id = $2")
        .bind(dec!(10))
        .bind(harness.account_id)
        .execute(&harness.pool)
        .await
        .expect("leverage update should succeed");
    harness.leverage = dec!(10);
    harness
}

/// Wrap an executed Trade as a `Position` for `Strategy::on_open_positions`.
fn position_from_trade(trade: &Trade) -> Position {
    Position { trade: trade.clone() }
}

/// Common assertions on a closed trade after `trader.close_position`.
/// `direction` is the original entry direction so we can pick the
/// expected exit-side (Long closes at bid, Short closes at ask).
#[allow(clippy::too_many_arguments)]
fn assert_closed_trade(
    closed: &Trade,
    direction: Direction,
    expected_reason: ExitReason,
    entry_price: Decimal,
    exit_bid: Decimal,
    exit_ask: Decimal,
    quantity: Decimal,
    pnl_should_be_positive: bool,
) {
    assert_eq!(closed.status, TradeStatus::Closed, "trade must be Closed");
    assert_eq!(
        closed.exit_reason,
        Some(expected_reason),
        "exit_reason must be {expected_reason:?}"
    );

    let expected_exit_price = match direction {
        Direction::Long => exit_bid,
        Direction::Short => exit_ask,
    };
    assert_eq!(
        closed.exit_price.expect("exit_price must be set"),
        expected_exit_price,
        "exit_price must equal {} side ({})",
        if matches!(direction, Direction::Long) { "bid" } else { "ask" },
        expected_exit_price,
    );

    let price_diff = match direction {
        Direction::Long => expected_exit_price - entry_price,
        Direction::Short => entry_price - expected_exit_price,
    };
    let expected_pnl = truncate_yen(price_diff * quantity);
    assert_eq!(
        closed.pnl_amount.expect("pnl_amount must be set"),
        expected_pnl,
        "pnl_amount must equal truncated price_diff × qty"
    );
    if pnl_should_be_positive {
        assert!(
            expected_pnl > Decimal::ZERO,
            "expected positive pnl, got {expected_pnl}"
        );
    } else {
        assert!(
            expected_pnl < Decimal::ZERO,
            "expected negative pnl, got {expected_pnl}"
        );
    }
}

// ─── Strategy drivers ──────────────────────────────────────────────────────

/// Load CSV fixture, set price store from the trigger candle's bid/ask,
/// drive strategy with warmup + trigger, return entry signal.
async fn drive_bb_entry(
    harness: &PipelineHarness,
    fixture: &str,
) -> (BbMeanRevertV1, Signal) {
    let mut strategy = BbMeanRevertV1::new(
        "test_strategy".to_string(),
        vec![Pair::new(PAIR)],
    );
    let events = load_events_from_csv(
        &fixtures_dir().join(fixture),
        EXCHANGE,
        PAIR,
        "M5",
    );
    let (warmup_events, trigger) = events.split_at(events.len() - 1);
    let trigger_event = trigger[0].clone();
    let bid = trigger_event.candle.best_bid.expect("fixture must carry bid");
    let ask = trigger_event.candle.best_ask.expect("fixture must carry ask");
    harness.set_market(bid, ask).await;
    let warmup_candles: Vec<_> = warmup_events.iter().map(|e| e.candle.clone()).collect();
    let signal = harness
        .drive_strategy(&mut strategy, &warmup_candles, &trigger_event.candle)
        .await
        .expect("bb fixture must produce a signal");
    (strategy, signal)
}

async fn drive_donchian_entry(
    harness: &PipelineHarness,
    fixture: &str,
) -> (DonchianTrendV1, Signal) {
    let mut strategy = DonchianTrendV1::new(
        "test_strategy".to_string(),
        vec![Pair::new(PAIR)],
    );
    let events = load_events_from_csv(
        &fixtures_dir().join(fixture),
        EXCHANGE,
        PAIR,
        "H1",
    );
    let (warmup_events, trigger) = events.split_at(events.len() - 1);
    let trigger_event = trigger[0].clone();
    let bid = trigger_event.candle.best_bid.expect("fixture must carry bid");
    let ask = trigger_event.candle.best_ask.expect("fixture must carry ask");
    harness.set_market(bid, ask).await;
    let warmup_candles: Vec<_> = warmup_events.iter().map(|e| e.candle.clone()).collect();
    let signal = harness
        .drive_strategy(&mut strategy, &warmup_candles, &trigger_event.candle)
        .await
        .expect("donchian fixture must produce a signal");
    (strategy, signal)
}

async fn drive_donchian_evolve_entry(
    harness: &PipelineHarness,
    fixture: &str,
) -> (DonchianTrendEvolveV1, Signal) {
    let mut strategy = DonchianTrendEvolveV1::new(
        "test_strategy".to_string(),
        vec![Pair::new(PAIR)],
        serde_json::json!({}),
    );
    let events = load_events_from_csv(
        &fixtures_dir().join(fixture),
        EXCHANGE,
        PAIR,
        "H1",
    );
    let (warmup_events, trigger) = events.split_at(events.len() - 1);
    let trigger_event = trigger[0].clone();
    let bid = trigger_event.candle.best_bid.expect("fixture must carry bid");
    let ask = trigger_event.candle.best_ask.expect("fixture must carry ask");
    harness.set_market(bid, ask).await;
    let warmup_candles: Vec<_> = warmup_events.iter().map(|e| e.candle.clone()).collect();
    let signal = harness
        .drive_strategy(&mut strategy, &warmup_candles, &trigger_event.candle)
        .await
        .expect("donchian_evolve fixture must produce a signal");
    (strategy, signal)
}

async fn drive_squeeze_entry(
    harness: &PipelineHarness,
    fixture: &str,
) -> (SqueezeMomentumV1, Signal) {
    let mut strategy = SqueezeMomentumV1::new(
        "test_strategy".to_string(),
        vec![Pair::new(PAIR)],
    );
    let events = load_events_from_csv(
        &fixtures_dir().join(fixture),
        EXCHANGE,
        PAIR,
        "H1",
    );
    let (warmup_events, trigger) = events.split_at(events.len() - 1);
    let trigger_event = trigger[0].clone();
    let bid = trigger_event.candle.best_bid.expect("fixture must carry bid");
    let ask = trigger_event.candle.best_ask.expect("fixture must carry ask");
    harness.set_market(bid, ask).await;
    let warmup_candles: Vec<_> = warmup_events.iter().map(|e| e.candle.clone()).collect();
    let signal = harness
        .drive_strategy(&mut strategy, &warmup_candles, &trigger_event.candle)
        .await
        .expect("squeeze fixture must produce a signal");
    (strategy, signal)
}

// ═══════════════════════════════════════════════════════════════════════════
// 1-2: bb_mean_revert MeanReached  (Long / Short)
// ═══════════════════════════════════════════════════════════════════════════

#[sqlx::test(migrations = "../../migrations")]
async fn bb_mean_revert_long_mean_reached_closes_with_correct_pnl(pool: PgPool) {
    let harness = build_harness(pool, "bb_long_mean_reached").await;

    // Drive Long entry from CSV fixture; trader fills @ ask=147.205.
    let (mut strategy, signal) = drive_bb_entry(&harness, "bb_long_entry.csv").await;
    assert_eq!(signal.direction, Direction::Long);
    let balance_before_open = harness.current_balance().await;
    let trade = harness.execute(&signal).await;
    let entry_price = trade.entry_price;
    let qty = trade.quantity;
    let balance_after_open = harness.current_balance().await;

    // Move market well above BB midline (~150) and past 1R distance.
    // SL distance = entry × stop_loss_pct ≤ entry × 0.03 ≈ 4.4 yen.
    // Set close=152.0 → unrealized = 4.795 ≥ sl_distance ⇒ 1R passes.
    // 152.0 ≥ midline (~150) ⇒ MeanReached fires.
    let exit_bid = dec!(151.995);
    let exit_ask = dec!(152.005);
    harness.set_market(exit_bid, exit_ask).await;

    // Feed the same bar through the strategy so it updates its indicators,
    // then ask for exits via on_open_positions.
    let exit_ts = DateTime::parse_from_rfc3339("2026-05-01T02:10:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_m5_event(dec!(152.000), dec!(152.100), dec!(151.900), exit_ts);
    let _ = strategy.on_price(&exit_event).await;

    let pos = position_from_trade(&trade);
    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert_eq!(exits.len(), 1, "BB Long expected 1 exit signal");
    assert_eq!(
        exits[0].trade_id, trade.id,
        "strategy must return the open trade's id (regression guard against id-mapping drift)"
    );
    assert_eq!(exits[0].reason, StrategyExitReason::MeanReached);

    // Map StrategyExitReason → ExitReason and close using the strategy-supplied
    // trade_id (so a wrong id from the strategy would fail the close).
    let exit_reason = exits[0].reason.to_exit_reason();
    assert_eq!(exit_reason, ExitReason::StrategyMeanReached);
    let closed = harness.close(exits[0].trade_id, exit_reason).await;

    assert_closed_trade(
        &closed,
        Direction::Long,
        ExitReason::StrategyMeanReached,
        entry_price,
        exit_bid,
        exit_ask,
        qty,
        true, // Long, exit > entry → positive pnl
    );

    // Ledger update: balance after close must reflect margin return + pnl
    // and any fees (matching the contract used in phase3_close_flow /
    // phase3_pipeline_e2e). fees is asserted == 0 explicitly so a future
    // non-zero fee model regression surfaces here.
    assert_eq!(closed.fees, dec!(0), "paper trade fees must be 0");
    let balance_after_close = harness.current_balance().await;
    let pnl = closed.pnl_amount.expect("pnl_amount must be set");
    assert_eq!(
        balance_after_close,
        balance_before_open + pnl - closed.fees,
        "balance after close = balance before open + pnl - fees"
    );
    assert!(
        balance_after_close > balance_after_open,
        "winning close must restore margin and add positive pnl"
    );
    let events = harness.events().await;
    assert!(
        events.iter().any(|e| e.event_type == "margin_lock"),
        "margin_lock event must exist after open"
    );
    assert!(
        events.iter().any(|e| e.event_type == "trade_close"),
        "trade_close event must exist after close"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn bb_mean_revert_short_mean_reached_closes_with_correct_pnl(pool: PgPool) {
    let harness = build_harness(pool, "bb_short_mean_reached").await;

    let (mut strategy, signal) = drive_bb_entry(&harness, "bb_short_entry.csv").await;
    assert_eq!(signal.direction, Direction::Short);
    let trade = harness.execute(&signal).await;
    let entry_price = trade.entry_price; // bid = 152.795
    let qty = trade.quantity;

    // Drop price below midline (~150) past 1R distance.
    let exit_bid = dec!(147.995);
    let exit_ask = dec!(148.005);
    harness.set_market(exit_bid, exit_ask).await;

    let exit_ts = DateTime::parse_from_rfc3339("2026-05-01T02:10:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_m5_event(dec!(148.000), dec!(148.100), dec!(147.900), exit_ts);
    let _ = strategy.on_price(&exit_event).await;

    let pos = position_from_trade(&trade);
    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert_eq!(exits.len(), 1, "BB Short expected 1 exit signal");
    assert_eq!(exits[0].trade_id, trade.id, "trade_id must match open trade");
    assert_eq!(exits[0].reason, StrategyExitReason::MeanReached);

    let closed = harness
        .close(exits[0].trade_id, exits[0].reason.to_exit_reason())
        .await;

    assert_closed_trade(
        &closed,
        Direction::Short,
        ExitReason::StrategyMeanReached,
        entry_price,
        exit_bid,
        exit_ask,
        qty,
        true, // Short, exit < entry → positive pnl
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 3-4: donchian_trend TrailingChannel  (Long / Short)
// ═══════════════════════════════════════════════════════════════════════════

#[sqlx::test(migrations = "../../migrations")]
async fn donchian_long_trailing_channel_closes_with_correct_pnl(pool: PgPool) {
    let harness = build_harness(pool, "donchian_long_trailing").await;

    let (mut strategy, signal) =
        drive_donchian_entry(&harness, "donchian_long_breakout.csv").await;
    assert_eq!(signal.direction, Direction::Long);
    let trade = harness.execute(&signal).await;
    let entry_price = trade.entry_price; // ask = 152.005
    let qty = trade.quantity;

    // Build the 10-bar exit-channel low at ~153.450 by feeding 12 elevated H1 bars.
    let entry_ts = DateTime::parse_from_rfc3339("2026-05-03T13:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for i in 0..12 {
        let ts = entry_ts + Duration::hours(i);
        let _ = strategy
            .on_price(&make_h1_event(
                dec!(153.500),
                dec!(153.550),
                dec!(153.450),
                ts,
            ))
            .await;
    }

    // Drop close to 153.0 < 10-bar low 153.450 → TrailingChannel fires.
    // Entry 152.005, SL pct ≤ 5% → sl_distance ≤ 7.6 → unrealized = 0.995.
    // We need 1R: choose price s.t. unrealized >= sl_distance. With dynamic
    // ATR-based SL the actual distance is much smaller (~1.0). At 153.0:
    // unrealized = 0.995 — this is right at threshold. Use 154.0 close for
    // safety: 154.0 still > 153.450 → trailing fires? No — trailing needs
    // close < channel_low. Re-strategy: the trailing exit *fires when close
    // < channel_low*. We need close in the band [entry+sl_distance, channel_low).
    // With sl_distance ≈ 1.0 and channel_low=153.450, we need close in
    // [153.005, 153.450). 153.4 works.
    let exit_bid = dec!(153.395);
    let exit_ask = dec!(153.405);
    harness.set_market(exit_bid, exit_ask).await;

    let drop_ts = entry_ts + Duration::hours(13);
    let exit_event = make_h1_event(dec!(153.400), dec!(153.450), dec!(153.350), drop_ts);
    let _ = strategy.on_price(&exit_event).await;

    let pos = position_from_trade(&trade);
    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert_eq!(exits.len(), 1, "Donchian Long expected 1 trailing exit");
    assert_eq!(exits[0].trade_id, trade.id, "trade_id must match open trade");
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);

    let closed = harness
        .close(exits[0].trade_id, exits[0].reason.to_exit_reason())
        .await;

    assert_closed_trade(
        &closed,
        Direction::Long,
        ExitReason::StrategyTrailingChannel,
        entry_price,
        exit_bid,
        exit_ask,
        qty,
        true, // Long, exit > entry → positive pnl
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn donchian_short_trailing_channel_closes_with_correct_pnl(pool: PgPool) {
    let harness = build_harness(pool, "donchian_short_trailing").await;

    let (mut strategy, signal) =
        drive_donchian_entry(&harness, "donchian_short_breakout.csv").await;
    assert_eq!(signal.direction, Direction::Short);
    let trade = harness.execute(&signal).await;
    let entry_price = trade.entry_price; // bid = 147.995
    let qty = trade.quantity;

    let entry_ts = DateTime::parse_from_rfc3339("2026-05-03T13:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    // Build 10-bar exit-channel high at ~146.550 by feeding 12 depressed bars.
    for i in 0..12 {
        let ts = entry_ts + Duration::hours(i);
        let _ = strategy
            .on_price(&make_h1_event(
                dec!(146.500),
                dec!(146.550),
                dec!(146.450),
                ts,
            ))
            .await;
    }

    // For Short: trailing fires on close > exit_high. Choose 146.6 ≥ 146.550.
    // sl_distance ≈ 1.0, entry≈148.0, unrealized at 146.6 = 1.395 ≥ 1.0 ⇒ 1R OK.
    let exit_bid = dec!(146.595);
    let exit_ask = dec!(146.605);
    harness.set_market(exit_bid, exit_ask).await;

    let drop_ts = entry_ts + Duration::hours(13);
    let exit_event = make_h1_event(dec!(146.600), dec!(146.650), dec!(146.550), drop_ts);
    let _ = strategy.on_price(&exit_event).await;

    let pos = position_from_trade(&trade);
    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert_eq!(exits.len(), 1, "Donchian Short expected 1 trailing exit");
    assert_eq!(exits[0].trade_id, trade.id, "trade_id must match open trade");
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);

    let closed = harness
        .close(exits[0].trade_id, exits[0].reason.to_exit_reason())
        .await;

    assert_closed_trade(
        &closed,
        Direction::Short,
        ExitReason::StrategyTrailingChannel,
        entry_price,
        exit_bid,
        exit_ask,
        qty,
        true,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 5: donchian_trend_evolve TrailingChannel  (Long)
// ═══════════════════════════════════════════════════════════════════════════

#[sqlx::test(migrations = "../../migrations")]
async fn donchian_evolve_long_trailing_channel_closes_with_correct_pnl(pool: PgPool) {
    let harness = build_harness(pool, "evolve_long_trailing").await;

    let (mut strategy, signal) =
        drive_donchian_evolve_entry(&harness, "donchian_long_breakout.csv").await;
    assert_eq!(signal.direction, Direction::Long);
    let trade = harness.execute(&signal).await;
    let entry_price = trade.entry_price;
    let qty = trade.quantity;

    let entry_ts = DateTime::parse_from_rfc3339("2026-05-03T13:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for i in 0..12 {
        let ts = entry_ts + Duration::hours(i);
        let _ = strategy
            .on_price(&make_h1_event(
                dec!(153.500),
                dec!(153.550),
                dec!(153.450),
                ts,
            ))
            .await;
    }

    // Same trailing exit as donchian_trend (default-fallback params).
    let exit_bid = dec!(153.395);
    let exit_ask = dec!(153.405);
    harness.set_market(exit_bid, exit_ask).await;
    let drop_ts = entry_ts + Duration::hours(13);
    let exit_event = make_h1_event(dec!(153.400), dec!(153.450), dec!(153.350), drop_ts);
    let _ = strategy.on_price(&exit_event).await;

    let pos = position_from_trade(&trade);
    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert_eq!(
        exits.len(),
        1,
        "DonchianTrendEvolve Long expected 1 trailing exit"
    );
    assert_eq!(exits[0].trade_id, trade.id, "trade_id must match open trade");
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);

    let closed = harness
        .close(exits[0].trade_id, exits[0].reason.to_exit_reason())
        .await;

    assert_closed_trade(
        &closed,
        Direction::Long,
        ExitReason::StrategyTrailingChannel,
        entry_price,
        exit_bid,
        exit_ask,
        qty,
        true,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 6-7: squeeze_momentum TrailingMa  (Long / Short)
// ═══════════════════════════════════════════════════════════════════════════

#[sqlx::test(migrations = "../../migrations")]
async fn squeeze_long_trailing_ma_closes_with_correct_pnl(pool: PgPool) {
    let harness = build_harness(pool, "squeeze_long_trailing_ma").await;

    let (mut strategy, signal) =
        drive_squeeze_entry(&harness, "squeeze_long_entry.csv").await;
    assert_eq!(signal.direction, Direction::Long);
    let trade = harness.execute(&signal).await;
    let entry_price = trade.entry_price; // ask = 151.505
    let qty = trade.quantity;

    // Squeeze chandelier exit requires (1) bars_held >= DELAY_BARS=3 from
    // entry_at, (2) 1R reached, (3) close < chandelier stop. The strategy
    // counts bars by `c.timestamp >= entry_at_floored_to_hour`, so the
    // synthesized exit bars MUST start AFTER trade.entry_at. We use
    // trade.entry_at as the reference and feed bars at +1h, +2h, …
    let entry_ts = trade
        .entry_at
        .with_nanosecond(0)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_minute(0)
        .unwrap();
    // Feed 20 H1 bars of rising prices to build Chandelier history past DELAY_BARS=3.
    // Mirrors `phase3_squeeze_momentum::warmup_long_with_chandelier_history`.
    for i in 1..=20 {
        let ts = entry_ts + Duration::hours(i);
        let p = dec!(152.000) + Decimal::from(i) * dec!(0.100);
        let _ = strategy
            .on_price(&make_h1_event(p, p + dec!(0.050), p - dec!(0.050), ts))
            .await;
    }

    // After the 20 rising bars from 152.1→154.0, the last 22 bars yield:
    //   highest_high(22) ≈ 154.05 (from the last fed bar i=20)
    //   ATR(14) ≈ 0.1 (small synthetic-bar TR)
    //   chandelier_stop = 154.05 - 0.1 × 3 = 153.75
    // Setting exit_close=153.5 puts us below the stop → trailing fires.
    // sl_distance is small (CSV has very flat ATR → stop_loss_pct ≪ 1%) so
    // 1R is easy: unrealized = 153.5 - entry(~151.5) = ~2.0 ≫ sl_distance.
    // (We assert it explicitly below to keep the test self-checking.)
    let sl_distance = entry_price * signal.stop_loss_pct;
    let exit_close = dec!(153.500);
    assert!(
        exit_close - entry_price >= sl_distance,
        "1R must be reachable for squeeze long exit: unrealized {} >= sl_distance {}",
        exit_close - entry_price,
        sl_distance
    );
    let exit_bid = exit_close - dec!(0.005);
    let exit_ask = exit_close + dec!(0.005);
    harness.set_market(exit_bid, exit_ask).await;
    let drop_ts = entry_ts + Duration::hours(21);
    let exit_event = make_h1_event(
        exit_close,
        exit_close + dec!(0.050),
        exit_close - dec!(0.050),
        drop_ts,
    );
    let _ = strategy.on_price(&exit_event).await;

    let pos = position_from_trade(&trade);
    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert_eq!(
        exits.len(),
        1,
        "Squeeze Long expected 1 Chandelier exit"
    );
    assert_eq!(exits[0].trade_id, trade.id, "trade_id must match open trade");
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingMa);

    let closed = harness
        .close(exits[0].trade_id, exits[0].reason.to_exit_reason())
        .await;

    assert_closed_trade(
        &closed,
        Direction::Long,
        ExitReason::StrategyTrailingMa,
        entry_price,
        exit_bid,
        exit_ask,
        qty,
        true,
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn squeeze_short_trailing_ma_closes_with_correct_pnl(pool: PgPool) {
    let harness = build_harness(pool, "squeeze_short_trailing_ma").await;

    let (mut strategy, signal) =
        drive_squeeze_entry(&harness, "squeeze_short_entry.csv").await;
    assert_eq!(signal.direction, Direction::Short);
    let trade = harness.execute(&signal).await;
    let entry_price = trade.entry_price; // bid = 148.495
    let qty = trade.quantity;

    // See squeeze_long_trailing_ma — bar timestamps must be after trade.entry_at
    // for the DELAY_BARS gating to count them.
    let entry_ts = trade
        .entry_at
        .with_nanosecond(0)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_minute(0)
        .unwrap();
    for i in 1..=20 {
        let ts = entry_ts + Duration::hours(i);
        let p = dec!(148.000) - Decimal::from(i) * dec!(0.100);
        let _ = strategy
            .on_price(&make_h1_event(p, p + dec!(0.050), p - dec!(0.050), ts))
            .await;
    }

    // After 20 falling bars (148.0 → 146.0) the last 22 give:
    //   lowest_low(22) ≈ 145.95, ATR(14) ≈ 0.1, chandelier_stop = 146.25.
    // exit_close = 146.5 > 146.25 → Short trailing fires.
    let sl_distance = entry_price * signal.stop_loss_pct;
    let exit_close = dec!(146.500);
    assert!(
        entry_price - exit_close >= sl_distance,
        "1R must be reachable for squeeze short exit: unrealized {} >= sl_distance {}",
        entry_price - exit_close,
        sl_distance
    );
    let exit_bid = exit_close - dec!(0.005);
    let exit_ask = exit_close + dec!(0.005);
    harness.set_market(exit_bid, exit_ask).await;
    let drop_ts = entry_ts + Duration::hours(21);
    let exit_event = make_h1_event(
        exit_close,
        exit_close + dec!(0.050),
        exit_close - dec!(0.050),
        drop_ts,
    );
    let _ = strategy.on_price(&exit_event).await;

    let pos = position_from_trade(&trade);
    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert_eq!(
        exits.len(),
        1,
        "Squeeze Short expected 1 Chandelier exit"
    );
    assert_eq!(exits[0].trade_id, trade.id, "trade_id must match open trade");
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingMa);

    let closed = harness
        .close(exits[0].trade_id, exits[0].reason.to_exit_reason())
        .await;

    assert_closed_trade(
        &closed,
        Direction::Short,
        ExitReason::StrategyTrailingMa,
        entry_price,
        exit_bid,
        exit_ask,
        qty,
        true,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 8: Time-limit close — verify trader.close_position(StrategyTimeLimit)
// ═══════════════════════════════════════════════════════════════════════════
//
// SCOPE: this test ONLY verifies the executor side of the time-limit chain.
//
//   trader.close_position(id, ExitReason::StrategyTimeLimit)
//     → closed Trade { exit_reason = StrategyTimeLimit, exit_price = bid,
//                      pnl from price diff }
//
// It does NOT exercise the position-monitor logic that decides "max_hold_until
// has passed → fire close". That detection path is the monitor's job and is
// covered separately (see `phase3_monitoring.rs`). Here we deliberately
// short-circuit straight to `close_position` because the goal of Task 6 is
// the strategy/exit-reason → trader connection, not the monitor's time-keeping.

#[sqlx::test(migrations = "../../migrations")]
async fn time_limit_close_persists_reason_and_market_bid(pool: PgPool) {
    let harness = build_harness(pool, "time_limit_exit").await;

    // Open a Long via a hand-built signal carrying max_hold_until.
    harness.set_market(dec!(149.995), dec!(150.005)).await;
    let signal = Signal {
        strategy_name: "test_strategy".to_string(),
        pair: Pair::new(PAIR),
        direction: Direction::Long,
        stop_loss_pct: dec!(0.005),
        take_profit_pct: None,
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        // Already in the past — represents a deadline the monitor would have
        // tripped on before this close fires.
        max_hold_until: Some(Utc::now() - Duration::hours(1)),
    };
    let trade = harness.execute(&signal).await;
    let entry_price = trade.entry_price;
    let qty = trade.quantity;
    assert_eq!(trade.max_hold_until, signal.max_hold_until);

    // No price move — close at current market. PnL ≈ 0 (slightly negative
    // from spread crossing: Long fills @ ask, closes @ bid).
    let exit_bid = dec!(149.995);
    let exit_ask = dec!(150.005);
    harness.set_market(exit_bid, exit_ask).await;

    // The mapping that the position monitor performs in production:
    let exit_reason = StrategyExitReason::TimeLimit.to_exit_reason();
    assert_eq!(exit_reason, ExitReason::StrategyTimeLimit);
    let closed = harness.close(trade.id, exit_reason).await;

    assert_closed_trade(
        &closed,
        Direction::Long,
        ExitReason::StrategyTimeLimit,
        entry_price,
        exit_bid,
        exit_ask,
        qty,
        false, // exit_bid (149.995) < entry_ask (150.005) → small loss from spread
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 9: SL-hit close — verify trader.close_position(SlHit)
// ═══════════════════════════════════════════════════════════════════════════
//
// SCOPE (same as test 8): this only verifies the executor side. The price
// monitor that decides "bid < SL → fire close" is covered separately in
// `phase3_monitoring.rs`. Here we move bid below SL purely as scenario
// shape, then call `close_position(SlHit)` directly to assert that the
// closed trade record carries `ExitReason::SlHit`, the market bid as
// exit_price, and the corresponding (negative) pnl.

#[sqlx::test(migrations = "../../migrations")]
async fn sl_hit_close_persists_reason_and_market_bid(pool: PgPool) {
    let harness = build_harness(pool, "sl_hit_close").await;

    // Open a Long with a 0.5% SL distance.
    harness.set_market(dec!(149.995), dec!(150.005)).await;
    let signal = Signal {
        strategy_name: "test_strategy".to_string(),
        pair: Pair::new(PAIR),
        direction: Direction::Long,
        stop_loss_pct: dec!(0.005),
        take_profit_pct: None,
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        max_hold_until: None,
    };
    let trade = harness.execute(&signal).await;
    let entry_price = trade.entry_price; // 150.005
    let qty = trade.quantity;
    // entry × (1 - 0.005) = 150.005 × 0.995 = 149.254975 (no rounding;
    // Trader::execute stores the SL price unrounded).
    let sl = trade.stop_loss;
    assert!(sl < entry_price);

    // Move bid below SL — production's price monitor would fire here. We
    // call close_position directly to test the executor handoff.
    let exit_bid = sl - dec!(0.010);
    let exit_ask = sl + dec!(0.000);
    harness.set_market(exit_bid, exit_ask).await;

    let closed = harness.close(trade.id, ExitReason::SlHit).await;

    assert_closed_trade(
        &closed,
        Direction::Long,
        ExitReason::SlHit,
        entry_price,
        exit_bid,
        exit_ask,
        qty,
        false, // SL hit → loss
    );
    assert!(
        closed.exit_price.unwrap() < entry_price,
        "SL hit must close below entry for Long"
    );
}
