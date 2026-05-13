//! Phase 3: Strategy × Exchange × Direction full-pipeline e2e tests.
//!
//! Each test drives the full pipeline:
//!   warmup candles → strategy.on_price → signal → trader.execute → trade
//!   fields → margin_lock event → close_position → exit_price/pnl/balance.
//!
//! Coverage: 4 strategies × 2 exchanges × 2 directions = 16 tests.
//!
//! - bb_mean_revert (M5)
//! - donchian_trend (H1)
//! - donchian_trend_evolve (H1)
//! - squeeze_momentum (H1)
//!
//! Each strategy runs on {BitflyerCfd FX_BTC_JPY, GmoFx USD_JPY} × {Long, Short}.
//!
//! `swing_llm` is intentionally excluded — D1 timeframe + Gemini LLM
//! dependency makes it a separate test concern.

use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, TradeStatus};
use auto_trader_integration_tests::helpers::pipeline::{PipelineHarness, PipelineHarnessConfig};
use auto_trader_integration_tests::helpers::sizing_invariants;
use auto_trader_integration_tests::helpers::trade_flow::{fixtures_dir, load_events_from_csv};
use auto_trader_strategy::bb_mean_revert::BbMeanRevertV1;
use auto_trader_strategy::donchian_trend::DonchianTrendV1;
use auto_trader_strategy::donchian_trend_evolve::DonchianTrendEvolveV1;
use auto_trader_strategy::squeeze_momentum::SqueezeMomentumV1;
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;
use sqlx::PgPool;

// ─── Constants ─────────────────────────────────────────────────────────────

const BTC_PAIR: &str = "FX_BTC_JPY";
const USD_PAIR: &str = "USD_JPY";

const BITFLYER_Y: Decimal = dec!(0.50); // 維持率 50% — bitFlyer Crypto CFD
const GMO_Y: Decimal = dec!(1.00); // 維持率 100% — GMO 外為 FX

const BTC_MIN_LOT: Decimal = dec!(0.001);
const USD_MIN_LOT: Decimal = dec!(1);

const SEED_BALANCE: i64 = 30_000;

// ─── Helpers ───────────────────────────────────────────────────────────────

/// Override the seeded leverage (default 2 from `seed_trading_account`) so
/// gmo_fx flow tests can run with the production-realistic 10x leverage.
async fn override_leverage(harness: &mut PipelineHarness, leverage: Decimal) {
    sqlx::query("UPDATE trading_accounts SET leverage = $1 WHERE id = $2")
        .bind(leverage)
        .bind(harness.account_id)
        .execute(&harness.pool)
        .await
        .expect("leverage update should succeed");
    harness.leverage = leverage;
}

/// Truncate-to-zero rounding to whole yen, mirroring `truncate_yen` in
/// the trader. Required when comparing against `pnl_amount` written to DB.
fn truncate_yen(x: Decimal) -> Decimal {
    x.round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

/// Scenario inputs for a single pipeline test.
struct Scenario {
    exchange: Exchange,
    pair_str: &'static str,
    /// Numeric leverage to enforce on the seeded account before execute.
    leverage: Decimal,
    /// Broker liquidation margin level (Y).
    y: Decimal,
    /// Min order size for the pair.
    min_lot: Decimal,
    /// CSV fixture filename (under `fixtures/phase3/`).
    fixture: &'static str,
    /// CSV timeframe label ("M5" or "H1").
    timeframe: &'static str,
    /// Strategy DB name (also used in PipelineHarness config).
    strategy_name: &'static str,
    direction: Direction,
}

/// All tests share this skeleton: build harness → drive strategy → execute
/// → assert open invariants → close winning → assert exit + balance.
///
/// The signal-emission step is parameterised by a closure so each strategy
/// can plug in its own builder.
async fn run_pipeline_test<F>(pool: PgPool, scenario: Scenario, account_label: &str, drive: F)
where
    F: AsyncFnOnce(&PipelineHarness, &Scenario) -> auto_trader_core::types::Signal,
{
    // 1. Build harness
    let mut harness = PipelineHarness::new(
        pool,
        PipelineHarnessConfig {
            account_name: account_label.to_string(),
            exchange: scenario.exchange,
            pair_str: scenario.pair_str.to_string(),
            strategy: scenario.strategy_name.to_string(),
            balance: SEED_BALANCE,
            liquidation_margin_level: scenario.y,
            min_order_size: scenario.min_lot,
        },
    )
    .await;

    if scenario.leverage != harness.leverage {
        override_leverage(&mut harness, scenario.leverage).await;
    }

    // 2. Strategy-driven signal — direction must match scenario.
    let signal = drive(&harness, &scenario).await;
    assert_eq!(
        signal.direction, scenario.direction,
        "strategy {} on {:?} {} should emit {:?}",
        scenario.strategy_name, scenario.exchange, scenario.pair_str, scenario.direction
    );
    assert!(
        signal.stop_loss_pct > Decimal::ZERO && signal.stop_loss_pct <= dec!(0.05),
        "stop_loss_pct must be in (0, 5%], got {}",
        signal.stop_loss_pct
    );

    // 3. Snapshot balance before open.
    let balance_before = harness.current_balance().await;
    assert_eq!(balance_before, Decimal::from(SEED_BALANCE));

    // 4. Execute the signal through the trader.
    let trade = harness.execute(&signal).await;

    // 5. The hint price (and dry-run fill price) is ask for Long, bid for
    //    Short. Read from the price store using the same code path.
    use auto_trader_market::price_store::FeedKey;
    let feed_key = FeedKey::new(harness.exchange, harness.pair.clone());
    let (entry_bid, entry_ask) = harness
        .price_store
        .latest_bid_ask(&feed_key)
        .await
        .expect("price store must have entry bid/ask");
    let entry_price = match scenario.direction {
        Direction::Long => entry_ask,
        Direction::Short => entry_bid,
    };
    assert_eq!(trade.entry_price, entry_price, "fill price = bid/ask side");

    // 6. Quantity must match the broker-aware sizing formula.
    let expected_qty = sizing_invariants::expected_quantity(
        balance_before,
        scenario.leverage,
        signal.allocation_pct,
        signal.stop_loss_pct,
        scenario.y,
        entry_price,
        scenario.min_lot,
    )
    .expect("scenario should produce a sizeable order");
    assert_eq!(
        trade.quantity, expected_qty,
        "qty must match expected_quantity for {:?} {:?} sl_pct={}",
        scenario.exchange, scenario.direction, signal.stop_loss_pct
    );
    assert!(
        trade.quantity > Decimal::ZERO,
        "quantity must be positive after sizer truncation"
    );

    // 7. Stop-loss price = entry × (1 ∓ sl_pct).
    assert_eq!(
        trade.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            entry_price,
            scenario.direction,
            signal.stop_loss_pct,
        ),
        "stop_loss price must be derived from fill price"
    );

    // 8. Misc trade fields.
    assert_eq!(trade.leverage, scenario.leverage);
    assert_eq!(trade.status, TradeStatus::Open);
    assert_eq!(trade.fees, Decimal::ZERO);
    assert_eq!(trade.exchange, scenario.exchange);
    assert_eq!(trade.pair, harness.pair);
    assert_eq!(trade.direction, scenario.direction);
    assert!(trade.exit_price.is_none());
    assert!(trade.pnl_amount.is_none());

    // 9. account_events must contain a margin_lock with amount = -margin.
    let expected_margin =
        sizing_invariants::expected_margin_lock(trade.quantity, trade.entry_price, trade.leverage);
    assert!(
        expected_margin > Decimal::ZERO,
        "expected_margin must be positive"
    );
    let events = harness.events().await;
    let margin_lock = events
        .iter()
        .find(|e| e.event_type == "margin_lock")
        .expect("margin_lock event must exist after execute");
    assert_eq!(
        margin_lock.amount, -expected_margin,
        "margin_lock amount = -expected_margin (signed outflow)"
    );

    // 10. current_balance reduced by margin.
    let balance_after_open = harness.current_balance().await;
    assert_eq!(
        balance_after_open,
        balance_before - expected_margin,
        "balance after open = balance - margin"
    );

    // 11. The new sizing invariant: post-SL margin level must stay >= Y.
    sizing_invariants::assert_post_sl_margin_level_at_least_y(&trade, balance_before, scenario.y);

    // 12. Move the market in the trade's favour and close at TP.
    //     Use a 1.5% move so the close is clearly in profit but well within
    //     the SL distance for any reasonable strategy.
    let exit_move = dec!(0.015);
    let (exit_bid, exit_ask) = match scenario.direction {
        Direction::Long => {
            // Long winning: price rises ⇒ both bid and ask rise.
            let new_bid = entry_bid * (Decimal::ONE + exit_move);
            let new_ask = entry_ask * (Decimal::ONE + exit_move);
            (new_bid, new_ask)
        }
        Direction::Short => {
            // Short winning: price falls ⇒ both bid and ask fall.
            let new_bid = entry_bid * (Decimal::ONE - exit_move);
            let new_ask = entry_ask * (Decimal::ONE - exit_move);
            (new_bid, new_ask)
        }
    };
    harness.set_market(exit_bid, exit_ask).await;

    let closed = harness.close(trade.id, ExitReason::TpHit).await;

    // 13. Closed trade fields.
    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(closed.exit_reason, Some(ExitReason::TpHit));
    let expected_exit_price = match scenario.direction {
        Direction::Long => exit_bid,  // close-long fills at bid
        Direction::Short => exit_ask, // close-short fills at ask
    };
    assert_eq!(
        closed.exit_price.expect("exit_price must be set"),
        expected_exit_price,
    );

    // 14. PnL = price_diff × qty, truncated to whole yen by the trader.
    let raw_pnl = sizing_invariants::expected_pnl(
        trade.entry_price,
        expected_exit_price,
        trade.quantity,
        scenario.direction,
    );
    let expected_pnl = truncate_yen(raw_pnl);
    assert_eq!(
        closed.pnl_amount.expect("pnl_amount must be set"),
        expected_pnl,
        "pnl must equal truncated price_diff × qty"
    );
    assert!(
        expected_pnl > Decimal::ZERO,
        "winning close should produce positive pnl, got {expected_pnl}"
    );

    // 15. balance after close = balance_after_open + margin_returned + pnl.
    let balance_after_close = harness.current_balance().await;
    assert_eq!(
        balance_after_close,
        balance_after_open + expected_margin + expected_pnl,
        "balance after close = balance_after_open + margin + pnl"
    );
    assert_eq!(
        balance_after_close,
        balance_before + expected_pnl,
        "alternative: balance_after_close = balance_before + pnl"
    );
}

// ─── Strategy drivers ──────────────────────────────────────────────────────

/// Load the fixture, set the price store from the trigger candle's bid/ask,
/// and return `(warmup_candles, trigger_candle)` ready for `drive_strategy`.
///
/// For BTC pairs, candle prices are scaled by ~75,000× so they live in the
/// realistic BTC/JPY million-yen range instead of the ~150 fixture range
/// (which is shaped for USD/JPY). All in-tree strategies are scale-invariant
/// — they evaluate %-based bands, ATR-relative breakouts, etc. — so the
/// scaled candles still trigger the same signals while exercising
/// realistic min_lot / spread / magnitude paths.
async fn prepare_candles(
    harness: &PipelineHarness,
    scenario: &Scenario,
) -> (
    Vec<auto_trader_core::types::Candle>,
    auto_trader_core::types::Candle,
) {
    let mut events = load_events_from_csv(
        &fixtures_dir().join(scenario.fixture),
        scenario.exchange,
        scenario.pair_str,
        scenario.timeframe,
    );
    assert!(events.len() >= 2, "fixture {} too short", scenario.fixture);

    // Scale prices into realistic BTC/JPY range (millions of yen) when the
    // scenario is the BTC pair. Strategies are scale-invariant so signal
    // outcomes are preserved.
    if scenario.pair_str == BTC_PAIR {
        const BTC_SCALE: rust_decimal::Decimal = rust_decimal_macros::dec!(75_000);
        for event in events.iter_mut() {
            event.candle.open *= BTC_SCALE;
            event.candle.high *= BTC_SCALE;
            event.candle.low *= BTC_SCALE;
            event.candle.close *= BTC_SCALE;
            if let Some(bid) = event.candle.best_bid {
                event.candle.best_bid = Some(bid * BTC_SCALE);
            }
            if let Some(ask) = event.candle.best_ask {
                event.candle.best_ask = Some(ask * BTC_SCALE);
            }
        }
    }

    let (warmup_events, trigger_events) = events.split_at(events.len() - 1);
    let trigger_event = trigger_events[0].clone();

    // Set price store from trigger candle's bid/ask so `trader.execute`
    // sees a fill price aligned with the signal moment.
    let bid = trigger_event
        .candle
        .best_bid
        .expect("fixture must carry bid");
    let ask = trigger_event
        .candle
        .best_ask
        .expect("fixture must carry ask");
    harness.set_market(bid, ask).await;

    let warmup_candles: Vec<_> = warmup_events.iter().map(|e| e.candle.clone()).collect();
    (warmup_candles, trigger_event.candle)
}

async fn drive_bb(
    harness: &PipelineHarness,
    scenario: &Scenario,
) -> auto_trader_core::types::Signal {
    let mut strategy = BbMeanRevertV1::new(
        scenario.strategy_name.to_string(),
        vec![Pair::new(scenario.pair_str)],
    );
    let (warmup, trigger) = prepare_candles(harness, scenario).await;
    harness
        .drive_strategy(&mut strategy, &warmup, &trigger)
        .await
        .expect("bb_mean_revert fixture must produce a signal")
}

async fn drive_donchian(
    harness: &PipelineHarness,
    scenario: &Scenario,
) -> auto_trader_core::types::Signal {
    let mut strategy = DonchianTrendV1::new(
        scenario.strategy_name.to_string(),
        vec![Pair::new(scenario.pair_str)],
    );
    let (warmup, trigger) = prepare_candles(harness, scenario).await;
    harness
        .drive_strategy(&mut strategy, &warmup, &trigger)
        .await
        .expect("donchian_trend fixture must produce a signal")
}

async fn drive_donchian_evolve(
    harness: &PipelineHarness,
    scenario: &Scenario,
) -> auto_trader_core::types::Signal {
    // Default-fallback params (`{}`) → identical behaviour to baseline V1,
    // which is exactly what we want for the e2e pipeline matrix.
    let mut strategy = DonchianTrendEvolveV1::new(
        scenario.strategy_name.to_string(),
        vec![Pair::new(scenario.pair_str)],
        serde_json::json!({}),
    );
    let (warmup, trigger) = prepare_candles(harness, scenario).await;
    harness
        .drive_strategy(&mut strategy, &warmup, &trigger)
        .await
        .expect("donchian_trend_evolve fixture must produce a signal")
}

async fn drive_squeeze(
    harness: &PipelineHarness,
    scenario: &Scenario,
) -> auto_trader_core::types::Signal {
    let mut strategy = SqueezeMomentumV1::new(
        scenario.strategy_name.to_string(),
        vec![Pair::new(scenario.pair_str)],
    );
    let (warmup, trigger) = prepare_candles(harness, scenario).await;
    harness
        .drive_strategy(&mut strategy, &warmup, &trigger)
        .await
        .expect("squeeze_momentum fixture must produce a signal")
}

// ═══════════════════════════════════════════════════════════════════════════
// BB Mean Revert (M5) × {BitflyerCfd FX_BTC_JPY, GmoFx USD_JPY} × {L, S}
// ═══════════════════════════════════════════════════════════════════════════

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_bb_mean_revert_bitflyer_cfd_long(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::BitflyerCfd,
            pair_str: BTC_PAIR,
            leverage: dec!(2),
            y: BITFLYER_Y,
            min_lot: BTC_MIN_LOT,
            fixture: "bb_long_entry.csv",
            timeframe: "M5",
            strategy_name: "bb_mean_revert_v1",
            direction: Direction::Long,
        },
        "pipe_bb_btc_long",
        drive_bb,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_bb_mean_revert_bitflyer_cfd_short(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::BitflyerCfd,
            pair_str: BTC_PAIR,
            leverage: dec!(2),
            y: BITFLYER_Y,
            min_lot: BTC_MIN_LOT,
            fixture: "bb_short_entry.csv",
            timeframe: "M5",
            strategy_name: "bb_mean_revert_v1",
            direction: Direction::Short,
        },
        "pipe_bb_btc_short",
        drive_bb,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_bb_mean_revert_gmo_fx_long(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::GmoFx,
            pair_str: USD_PAIR,
            leverage: dec!(10),
            y: GMO_Y,
            min_lot: USD_MIN_LOT,
            fixture: "bb_long_entry.csv",
            timeframe: "M5",
            strategy_name: "bb_mean_revert_v1",
            direction: Direction::Long,
        },
        "pipe_bb_usd_long",
        drive_bb,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_bb_mean_revert_gmo_fx_short(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::GmoFx,
            pair_str: USD_PAIR,
            leverage: dec!(10),
            y: GMO_Y,
            min_lot: USD_MIN_LOT,
            fixture: "bb_short_entry.csv",
            timeframe: "M5",
            strategy_name: "bb_mean_revert_v1",
            direction: Direction::Short,
        },
        "pipe_bb_usd_short",
        drive_bb,
    )
    .await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Donchian Trend (H1) × {BitflyerCfd FX_BTC_JPY, GmoFx USD_JPY} × {L, S}
// ═══════════════════════════════════════════════════════════════════════════

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_donchian_trend_bitflyer_cfd_long(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::BitflyerCfd,
            pair_str: BTC_PAIR,
            leverage: dec!(2),
            y: BITFLYER_Y,
            min_lot: BTC_MIN_LOT,
            fixture: "donchian_long_breakout.csv",
            timeframe: "H1",
            strategy_name: "donchian_trend_v1",
            direction: Direction::Long,
        },
        "pipe_donchian_btc_long",
        drive_donchian,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_donchian_trend_bitflyer_cfd_short(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::BitflyerCfd,
            pair_str: BTC_PAIR,
            leverage: dec!(2),
            y: BITFLYER_Y,
            min_lot: BTC_MIN_LOT,
            fixture: "donchian_short_breakout.csv",
            timeframe: "H1",
            strategy_name: "donchian_trend_v1",
            direction: Direction::Short,
        },
        "pipe_donchian_btc_short",
        drive_donchian,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_donchian_trend_gmo_fx_long(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::GmoFx,
            pair_str: USD_PAIR,
            leverage: dec!(10),
            y: GMO_Y,
            min_lot: USD_MIN_LOT,
            fixture: "donchian_long_breakout.csv",
            timeframe: "H1",
            strategy_name: "donchian_trend_v1",
            direction: Direction::Long,
        },
        "pipe_donchian_usd_long",
        drive_donchian,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_donchian_trend_gmo_fx_short(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::GmoFx,
            pair_str: USD_PAIR,
            leverage: dec!(10),
            y: GMO_Y,
            min_lot: USD_MIN_LOT,
            fixture: "donchian_short_breakout.csv",
            timeframe: "H1",
            strategy_name: "donchian_trend_v1",
            direction: Direction::Short,
        },
        "pipe_donchian_usd_short",
        drive_donchian,
    )
    .await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Donchian Trend Evolve (H1) × {BitflyerCfd FX_BTC_JPY, GmoFx USD_JPY} × {L, S}
// ═══════════════════════════════════════════════════════════════════════════

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_donchian_evolve_bitflyer_cfd_long(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::BitflyerCfd,
            pair_str: BTC_PAIR,
            leverage: dec!(2),
            y: BITFLYER_Y,
            min_lot: BTC_MIN_LOT,
            fixture: "donchian_long_breakout.csv",
            timeframe: "H1",
            strategy_name: "donchian_trend_evolve_v1",
            direction: Direction::Long,
        },
        "pipe_evolve_btc_long",
        drive_donchian_evolve,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_donchian_evolve_bitflyer_cfd_short(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::BitflyerCfd,
            pair_str: BTC_PAIR,
            leverage: dec!(2),
            y: BITFLYER_Y,
            min_lot: BTC_MIN_LOT,
            fixture: "donchian_short_breakout.csv",
            timeframe: "H1",
            strategy_name: "donchian_trend_evolve_v1",
            direction: Direction::Short,
        },
        "pipe_evolve_btc_short",
        drive_donchian_evolve,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_donchian_evolve_gmo_fx_long(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::GmoFx,
            pair_str: USD_PAIR,
            leverage: dec!(10),
            y: GMO_Y,
            min_lot: USD_MIN_LOT,
            fixture: "donchian_long_breakout.csv",
            timeframe: "H1",
            strategy_name: "donchian_trend_evolve_v1",
            direction: Direction::Long,
        },
        "pipe_evolve_usd_long",
        drive_donchian_evolve,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_donchian_evolve_gmo_fx_short(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::GmoFx,
            pair_str: USD_PAIR,
            leverage: dec!(10),
            y: GMO_Y,
            min_lot: USD_MIN_LOT,
            fixture: "donchian_short_breakout.csv",
            timeframe: "H1",
            strategy_name: "donchian_trend_evolve_v1",
            direction: Direction::Short,
        },
        "pipe_evolve_usd_short",
        drive_donchian_evolve,
    )
    .await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Squeeze Momentum (H1) × {BitflyerCfd FX_BTC_JPY, GmoFx USD_JPY} × {L, S}
// ═══════════════════════════════════════════════════════════════════════════

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_squeeze_momentum_bitflyer_cfd_long(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::BitflyerCfd,
            pair_str: BTC_PAIR,
            leverage: dec!(2),
            y: BITFLYER_Y,
            min_lot: BTC_MIN_LOT,
            fixture: "squeeze_long_entry.csv",
            timeframe: "H1",
            strategy_name: "squeeze_momentum_v1",
            direction: Direction::Long,
        },
        "pipe_squeeze_btc_long",
        drive_squeeze,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_squeeze_momentum_bitflyer_cfd_short(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::BitflyerCfd,
            pair_str: BTC_PAIR,
            leverage: dec!(2),
            y: BITFLYER_Y,
            min_lot: BTC_MIN_LOT,
            fixture: "squeeze_short_entry.csv",
            timeframe: "H1",
            strategy_name: "squeeze_momentum_v1",
            direction: Direction::Short,
        },
        "pipe_squeeze_btc_short",
        drive_squeeze,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_squeeze_momentum_gmo_fx_long(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::GmoFx,
            pair_str: USD_PAIR,
            leverage: dec!(10),
            y: GMO_Y,
            min_lot: USD_MIN_LOT,
            fixture: "squeeze_long_entry.csv",
            timeframe: "H1",
            strategy_name: "squeeze_momentum_v1",
            direction: Direction::Long,
        },
        "pipe_squeeze_usd_long",
        drive_squeeze,
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_squeeze_momentum_gmo_fx_short(pool: PgPool) {
    run_pipeline_test(
        pool,
        Scenario {
            exchange: Exchange::GmoFx,
            pair_str: USD_PAIR,
            leverage: dec!(10),
            y: GMO_Y,
            min_lot: USD_MIN_LOT,
            fixture: "squeeze_short_entry.csv",
            timeframe: "H1",
            strategy_name: "squeeze_momentum_v1",
            direction: Direction::Short,
        },
        "pipe_squeeze_usd_short",
        drive_squeeze,
    )
    .await;
}
