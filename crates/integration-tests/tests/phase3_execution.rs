//! Phase 3B: Execution guards — PositionSizer, fill_open/fill_close, freshness gate.
//!
//! Trader/PositionSizer レベルのテスト。フル app 起動は不要。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Signal, TradeStatus};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::risk_gate::{GateDecision, eval_price_freshness};
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::{read_current_balance, seed_trading_account};
use auto_trader_integration_tests::helpers::sizing_invariants;
use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal_macros::dec;
use uuid::Uuid;

// =========================================================================
// PositionSizer tests
// =========================================================================

fn btc_sizer() -> PositionSizer {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
    PositionSizer::new(min_sizes)
}

/// PositionSizer 正常: balance=100000, entry=15M, leverage=2, allocation=1.0, SL=2%, Y=0.5
/// max_alloc = 1 / (0.5 + 2 × 0.02) = 1.85 → capped to allocation=1.0
/// qty = 100000 × 2 × 1.0 / 15000000 ≈ 0.013333 → truncated to 0.013
#[test]
fn position_sizer_normal() {
    let qty = btc_sizer().calculate_quantity(
        &Pair::new("FX_BTC_JPY"),
        dec!(100000),
        dec!(15000000),
        dec!(2),
        dec!(1.0),
        dec!(0.02),
        dec!(0.50),
    );
    assert_eq!(qty, Some(dec!(0.013)));
}

/// PositionSizer 残高不足: balance=1000 → min_lot 未満で None。
#[test]
fn position_sizer_insufficient_balance() {
    let qty = btc_sizer().calculate_quantity(
        &Pair::new("FX_BTC_JPY"),
        dec!(1000),
        dec!(15000000),
        dec!(2),
        dec!(1.0),
        dec!(0.02),
        dec!(0.50),
    );
    assert_eq!(
        qty, None,
        "balance=1000 should be below min_lot for BTC at 15M"
    );
}

/// PositionSizer 極小残高: balance=100 → 確実に min_lot 未満。
#[test]
fn position_sizer_below_min_lot() {
    let qty = btc_sizer().calculate_quantity(
        &Pair::new("FX_BTC_JPY"),
        dec!(100),
        dec!(15000000),
        dec!(2),
        dec!(1.0),
        dec!(0.02),
        dec!(0.50),
    );
    assert_eq!(qty, None, "tiny balance should not reach min_lot");
}

// =========================================================================
// fill_open / fill_close tests (dry_run=true, PriceStore ベース)
// =========================================================================

/// テスト用の PriceStore を bid=150, ask=151 で構築する。
async fn price_store_with_bid_ask(exchange: Exchange, pair: &str) -> Arc<PriceStore> {
    let feed_key = FeedKey::new(exchange, Pair::new(pair));
    let store = PriceStore::new(vec![feed_key.clone()]);
    store
        .update(
            feed_key,
            LatestTick {
                price: dec!(150.5),
                best_bid: Some(dec!(150)),
                best_ask: Some(dec!(151)),
                ts: Utc::now(),
            },
        )
        .await;
    store
}

fn make_signal(pair: &str, direction: Direction) -> Signal {
    Signal {
        strategy_name: "test_strategy".to_string(),
        pair: Pair::new(pair),
        direction,
        stop_loss_pct: dec!(0.02),
        take_profit_pct: Some(dec!(0.04)),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        max_hold_until: None,
    }
}

/// テスト用 Trader (dry_run=true) を構築する。
async fn make_dry_run_trader(
    pool: sqlx::PgPool,
    exchange: Exchange,
    account_id: Uuid,
    price_store: Arc<PriceStore>,
) -> Trader {
    // min_order_sizes: 対象ペアの min_lot を十分小さくして sizer が通るようにする
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let sizer = Arc::new(PositionSizer::new(min_sizes));

    let api = MockExchangeApiBuilder::new().build();
    let notifier = Arc::new(Notifier::new_disabled());

    Trader::new(
        pool,
        exchange,
        account_id,
        "test_account".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        true, // dry_run
    )
}

/// fill_open Long → ask price (151) で約定。
#[sqlx::test(migrations = "../../migrations")]
async fn fill_open_long_uses_ask_price(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "fill_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let price_store = price_store_with_bid_ask(exchange, "USD_JPY").await;
    let trader = make_dry_run_trader(pool.clone(), exchange, account_id, price_store).await;

    let balance_before_open = read_current_balance(&pool, account_id).await;
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    assert_eq!(
        trade.entry_price,
        dec!(151),
        "Long should fill at ask price"
    );
    assert_eq!(trade.direction, Direction::Long);
    assert_eq!(trade.status, TradeStatus::Open);
    // qty: balance=1_000_000, lev=2, Y=1.00, SL=0.02, alloc=1.0, entry=151, min_lot=1
    //      max_alloc = 1/(1.00 + 2×0.02) = 1/1.04
    //      raw = 1_000_000 × 2 × (1/1.04) / 151 ≈ 12735.39 → floor → 12735
    assert_eq!(
        trade.quantity,
        dec!(12735),
        "sizer: 1M × 2 × (1/1.04) / 151 → 12735"
    );
    // Enriched assertions (Task 7).
    assert_eq!(
        trade.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            trade.entry_price,
            signal.direction,
            signal.stop_loss_pct,
        ),
        "stop_loss must equal entry × (1 - SL%) for Long"
    );
    let tp_pct = signal.take_profit_pct.expect("signal has TP");
    assert_eq!(
        trade.take_profit,
        Some(sizing_invariants::expected_take_profit_price(
            trade.entry_price,
            signal.direction,
            tp_pct,
        )),
        "take_profit must equal entry × (1 + TP%) for Long"
    );
    assert_eq!(trade.leverage, dec!(2), "leverage from seeded account");
    assert_eq!(trade.fees, dec!(0), "no fees at trade open");
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before_open,
        dec!(1.00),
    );
}

/// fill_open Short → bid price (150) で約定。
#[sqlx::test(migrations = "../../migrations")]
async fn fill_open_short_uses_bid_price(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "fill_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let price_store = price_store_with_bid_ask(exchange, "USD_JPY").await;
    let trader = make_dry_run_trader(pool.clone(), exchange, account_id, price_store).await;

    let balance_before_open = read_current_balance(&pool, account_id).await;
    let signal = make_signal("USD_JPY", Direction::Short);
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    assert_eq!(
        trade.entry_price,
        dec!(150),
        "Short should fill at bid price"
    );
    assert_eq!(trade.direction, Direction::Short);
    // qty: balance=1_000_000, lev=2, Y=1.00, SL=0.02, alloc=1.0, entry=150, min_lot=1
    //      max_alloc = 1/1.04, raw = 1_000_000 × 2 × (1/1.04) / 150 ≈ 12820.51 → floor → 12820
    assert_eq!(
        trade.quantity,
        dec!(12820),
        "sizer: 1M × 2 × (1/1.04) / 150 → 12820"
    );
    // Enriched assertions (Task 7).
    assert_eq!(
        trade.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            trade.entry_price,
            signal.direction,
            signal.stop_loss_pct,
        ),
        "stop_loss must equal entry × (1 + SL%) for Short"
    );
    let tp_pct = signal.take_profit_pct.expect("signal has TP");
    assert_eq!(
        trade.take_profit,
        Some(sizing_invariants::expected_take_profit_price(
            trade.entry_price,
            signal.direction,
            tp_pct,
        )),
        "take_profit must equal entry × (1 - TP%) for Short"
    );
    assert_eq!(trade.leverage, dec!(2), "leverage from seeded account");
    assert_eq!(trade.fees, dec!(0), "no fees at trade open");
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before_open,
        dec!(1.00),
    );
}

/// fill_close Long → bid price (150) で決済。
#[sqlx::test(migrations = "../../migrations")]
async fn fill_close_long_uses_bid_price(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "fill_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let price_store = price_store_with_bid_ask(exchange, "USD_JPY").await;
    let trader = make_dry_run_trader(pool.clone(), exchange, account_id, price_store).await;

    let balance_before_open = read_current_balance(&pool, account_id).await;
    // まず Long ポジションを開く
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");
    // qty: 1M × 2 × (1/1.04) / 151 → 12735 (Long fills at ask=151)
    assert_eq!(
        trade.quantity,
        dec!(12735),
        "open qty preserved through close"
    );
    // Open-side enrichment.
    assert_eq!(
        trade.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            trade.entry_price,
            signal.direction,
            signal.stop_loss_pct,
        ),
    );
    assert_eq!(
        trade.take_profit,
        Some(sizing_invariants::expected_take_profit_price(
            trade.entry_price,
            signal.direction,
            signal.take_profit_pct.unwrap(),
        )),
    );
    assert_eq!(trade.leverage, dec!(2));
    assert_eq!(trade.fees, dec!(0));
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before_open,
        dec!(1.00),
    );

    // クローズ
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit)
        .await
        .expect("close should succeed");

    assert_eq!(
        closed.exit_price,
        Some(dec!(150)),
        "Long close should fill at bid price"
    );
    assert_eq!(closed.status, TradeStatus::Closed);
    assert!(closed.exit_reason == Some(ExitReason::SlHit));
    // qty unchanged after close
    assert_eq!(
        closed.quantity,
        dec!(12735),
        "close should not mutate quantity"
    );
    // Close-side enrichment.
    let exit_price = closed.exit_price.expect("exit_price must be set on Closed");
    let expected_pnl = sizing_invariants::expected_pnl(
        closed.entry_price,
        exit_price,
        closed.quantity,
        closed.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(
        closed.pnl_amount,
        Some(expected_pnl),
        "pnl_amount = TRUNC((exit - entry) × qty, 0)"
    );
    // Balance reflects the realised PnL (margin already released).
    let balance_after_close = read_current_balance(&pool, account_id).await;
    assert_eq!(
        balance_after_close,
        balance_before_open + expected_pnl - closed.fees,
        "balance = before_open + pnl - fees"
    );
}

/// fill_close Short → ask price (151) で決済。
#[sqlx::test(migrations = "../../migrations")]
async fn fill_close_short_uses_ask_price(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "fill_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let price_store = price_store_with_bid_ask(exchange, "USD_JPY").await;
    let trader = make_dry_run_trader(pool.clone(), exchange, account_id, price_store).await;

    let balance_before_open = read_current_balance(&pool, account_id).await;
    // まず Short ポジションを開く
    let signal = make_signal("USD_JPY", Direction::Short);
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");
    // qty: 1M × 2 × (1/1.04) / 150 → 12820 (Short fills at bid=150)
    assert_eq!(
        trade.quantity,
        dec!(12820),
        "open qty preserved through close"
    );
    // Open-side enrichment.
    assert_eq!(
        trade.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            trade.entry_price,
            signal.direction,
            signal.stop_loss_pct,
        ),
    );
    assert_eq!(
        trade.take_profit,
        Some(sizing_invariants::expected_take_profit_price(
            trade.entry_price,
            signal.direction,
            signal.take_profit_pct.unwrap(),
        )),
    );
    assert_eq!(trade.leverage, dec!(2));
    assert_eq!(trade.fees, dec!(0));
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before_open,
        dec!(1.00),
    );

    // クローズ
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");

    assert_eq!(
        closed.exit_price,
        Some(dec!(151)),
        "Short close should fill at ask price"
    );
    assert_eq!(closed.status, TradeStatus::Closed);
    assert!(closed.exit_reason == Some(ExitReason::TpHit));
    assert_eq!(
        closed.quantity,
        dec!(12820),
        "close should not mutate quantity"
    );
    // Close-side enrichment.
    let exit_price = closed.exit_price.expect("exit_price must be set on Closed");
    let expected_pnl = sizing_invariants::expected_pnl(
        closed.entry_price,
        exit_price,
        closed.quantity,
        closed.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(
        closed.pnl_amount,
        Some(expected_pnl),
        "pnl_amount = TRUNC((entry - exit) × qty, 0) for Short"
    );
    let balance_after_close = read_current_balance(&pool, account_id).await;
    assert_eq!(
        balance_after_close,
        balance_before_open + expected_pnl - closed.fees,
        "balance = before_open + pnl - fees"
    );
}

// =========================================================================
// Freshness gate tests
// =========================================================================

/// Freshness gate: age_secs > threshold → Reject。
#[test]
fn freshness_gate_reject() {
    let decision = eval_price_freshness(60, 120);
    assert!(
        matches!(decision, GateDecision::Reject(_)),
        "age 120s > threshold 60s should be rejected"
    );
}

/// Freshness gate: age_secs <= threshold → Pass。
#[test]
fn freshness_gate_pass() {
    let decision = eval_price_freshness(60, 30);
    assert!(
        matches!(decision, GateDecision::Pass),
        "age 30s <= threshold 60s should pass"
    );
}
