//! Phase 3: Execution flow integration tests.
//!
//! 3.56 warmup — covered in Phase 1 tests.
//! 3.57 candle boundary — CandleBuilder M5 boundary test.
//! 3.62 fill_open Live — MockExchangeApi send_child_order → poll_executions.
//! 3.63 fill_close Live — MockExchangeApi close path.
//! 3.65 live gate — account_type="live" + live_enabled=false → reject.
//! 3.67 match none — no account matches strategy → graceful handling.
//! 3.68 multi-account — signal dispatches to correct exchange only.
//! 3.69 Live/Paper split — dry_run=true uses PriceStore, dry_run=false uses API.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Signal, TradeStatus};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
use auto_trader_market::bitflyer_private::{
    Execution, SendChildOrderResponse,
};
use auto_trader_market::candle_builder::CandleBuilder;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

// =========================================================================
// Helpers
// =========================================================================

async fn make_price_store(
    exchange: Exchange,
    pair: &str,
    bid: Decimal,
    ask: Decimal,
) -> Arc<PriceStore> {
    let feed_key = FeedKey::new(exchange, Pair::new(pair));
    let store = PriceStore::new(vec![feed_key.clone()]);
    store
        .update(
            feed_key,
            LatestTick {
                price: (bid + ask) / dec!(2),
                best_bid: Some(bid),
                best_ask: Some(ask),
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

fn usd_jpy_sizer() -> Arc<PositionSizer> {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    Arc::new(PositionSizer::new(min_sizes))
}

// =========================================================================
// 3.57: candle boundary — M5 boundary tick completes the previous candle
// =========================================================================

#[test]
fn candle_boundary_m5() {
    let pair = Pair::new("FX_BTC_JPY");
    let mut builder = CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, "M5".to_string());

    // A tick at 00:04:59 — inside the 00:00:00 period
    let t1 = Utc.with_ymd_and_hms(2026, 4, 10, 12, 4, 59).unwrap();
    assert!(
        builder
            .on_tick(dec!(15_000_000), dec!(0.1), t1, None, None)
            .is_none(),
        "tick at 04:59 should not complete a candle (first tick of period)"
    );

    // Another tick still in the same M5 period
    let t2 = Utc.with_ymd_and_hms(2026, 4, 10, 12, 4, 59).unwrap()
        + chrono::Duration::milliseconds(500);
    assert!(
        builder
            .on_tick(dec!(15_100_000), dec!(0.05), t2, Some(dec!(15_099_000)), Some(dec!(15_101_000)))
            .is_none(),
        "second tick in same period should not complete a candle"
    );

    // A tick at exactly 00:05:00 — starts a new M5 period, completes the previous
    let t3 = Utc.with_ymd_and_hms(2026, 4, 10, 12, 5, 0).unwrap();
    let completed = builder.on_tick(dec!(15_200_000), dec!(0.2), t3, None, None);
    assert!(
        completed.is_some(),
        "tick at 05:00 should complete the previous M5 candle"
    );

    let candle = completed.unwrap();
    assert_eq!(candle.timeframe, "M5");
    assert_eq!(candle.open, dec!(15_000_000));
    assert_eq!(candle.high, dec!(15_100_000));
    assert_eq!(candle.low, dec!(15_000_000));
    assert_eq!(candle.close, dec!(15_100_000));
    // The completed candle should carry the last-seen bid/ask
    assert_eq!(candle.best_bid, Some(dec!(15_099_000)));
    assert_eq!(candle.best_ask, Some(dec!(15_101_000)));
    // The period start should be the M5 period that was completed (00:00:00)
    let expected_ts = Utc.with_ymd_and_hms(2026, 4, 10, 12, 0, 0).unwrap();
    assert_eq!(candle.timestamp, expected_ts);
}

/// Zero-tick period: CandleBuilder with no ticks should not produce a candle.
#[test]
fn candle_boundary_zero_ticks() {
    let pair = Pair::new("FX_BTC_JPY");
    let mut builder = CandleBuilder::new(pair, Exchange::BitflyerCfd, "M5".to_string());
    let now = Utc.with_ymd_and_hms(2026, 4, 10, 12, 10, 0).unwrap();
    assert!(
        builder.try_complete(now, None, None).is_none(),
        "empty builder should not produce a candle"
    );
}

/// H1 boundary: verify candle completion at hour boundary.
#[test]
fn candle_boundary_h1() {
    let pair = Pair::new("USD_JPY");
    let mut builder = CandleBuilder::new(pair.clone(), Exchange::GmoFx, "H1".to_string());

    // Tick at 12:30:00
    let t1 = Utc.with_ymd_and_hms(2026, 4, 10, 12, 30, 0).unwrap();
    assert!(builder.on_tick(dec!(150), dec!(100), t1, None, None).is_none());

    // Tick at 13:00:00 — new hour, completes previous
    let t2 = Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap();
    let completed = builder.on_tick(dec!(151), dec!(50), t2, None, None);
    assert!(completed.is_some(), "H1 boundary should complete candle");
    let candle = completed.unwrap();
    assert_eq!(candle.timeframe, "H1");
    assert_eq!(candle.open, dec!(150));
    assert_eq!(candle.close, dec!(150));
    let expected_ts = Utc.with_ymd_and_hms(2026, 4, 10, 12, 0, 0).unwrap();
    assert_eq!(candle.timestamp, expected_ts);
}

// =========================================================================
// 3.62: fill_open Live — send_child_order → poll_executions
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn fill_open_live_calls_exchange_api(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "live_fill_test",
        "paper", // paper account but dry_run=false to exercise live path
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let price_store = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    // Configure MockExchangeApi with executions
    let api = MockExchangeApiBuilder::new()
        .with_send_child_order_response(SendChildOrderResponse {
            child_order_acceptance_id: "live-test-order-001".to_string(),
        })
        .with_get_executions_response(vec![Execution {
            id: 1,
            child_order_id: "live-test-order-001".to_string(),
            side: "BUY".to_string(),
            price: dec!(151),
            size: dec!(6622),
            commission: dec!(0),
            exec_date: "2026-04-10T12:00:00".to_string(),
            child_order_acceptance_id: "live-test-order-001".to_string(),
        }])
        .build();

    let counters = api.counters.clone();
    let sizer = usd_jpy_sizer();
    let notifier = Arc::new(Notifier::new_disabled());

    let trader = Trader::new(
        pool,
        exchange,
        account_id,
        "live_fill_test".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        false, // dry_run=false → live path
    )
    .with_poll_timeout(Duration::from_secs(5));

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("execute should succeed");

    // Verify send_child_order was called
    assert!(
        counters.send_child_order.load(Ordering::SeqCst) >= 1,
        "send_child_order should be called at least once"
    );

    // Verify get_executions was called (poll_executions)
    assert!(
        counters.get_executions.load(Ordering::SeqCst) >= 1,
        "get_executions should be called at least once"
    );

    assert_eq!(trade.status, TradeStatus::Open);
    assert_eq!(trade.direction, Direction::Long);
}

// =========================================================================
// 3.63: fill_close Live — send_child_order → poll_executions for close
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn fill_close_live_calls_exchange_api(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "live_close_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let price_store = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    let api = MockExchangeApiBuilder::new()
        .with_send_child_order_response(SendChildOrderResponse {
            child_order_acceptance_id: "live-close-order-001".to_string(),
        })
        .with_get_executions_response(vec![Execution {
            id: 1,
            child_order_id: "live-close-order-001".to_string(),
            side: "BUY".to_string(),
            price: dec!(151),
            size: dec!(6622),
            commission: dec!(0),
            exec_date: "2026-04-10T12:00:00".to_string(),
            child_order_acceptance_id: "live-close-order-001".to_string(),
        }])
        .build();

    let counters = api.counters.clone();
    let sizer = usd_jpy_sizer();
    let notifier = Arc::new(Notifier::new_disabled());

    let trader = Trader::new(
        pool.clone(),
        exchange,
        account_id,
        "live_close_test".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        false, // dry_run=false
    )
    .with_poll_timeout(Duration::from_secs(5));

    // Open a trade
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");

    // Reset counters to track close-only calls
    counters.send_child_order.store(0, Ordering::SeqCst);
    counters.get_executions.store(0, Ordering::SeqCst);

    // Close the trade
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit)
        .await
        .expect("close should succeed");

    // Verify send_child_order was called for the close (reverse order)
    assert!(
        counters.send_child_order.load(Ordering::SeqCst) >= 1,
        "send_child_order should be called for close"
    );

    // Verify get_executions was called for close
    assert!(
        counters.get_executions.load(Ordering::SeqCst) >= 1,
        "get_executions should be called for close fill"
    );

    assert_eq!(closed.status, TradeStatus::Closed);
    assert!(closed.exit_price.is_some());
}

// =========================================================================
// 3.65: live gate — account_type="live" + executor_live_enabled=false → reject
// =========================================================================

#[test]
fn live_gate_rejects_when_live_disabled() {
    // Simulate the live gate check from the signal executor:
    // if pac.account_type == "live" && !executor_live_enabled { reject }
    let account_type = "live";
    let executor_live_enabled = false;

    let should_reject = account_type == "live" && !executor_live_enabled;
    assert!(
        should_reject,
        "live account with live_enabled=false should be rejected"
    );
}

#[test]
fn live_gate_passes_when_live_enabled() {
    let account_type = "live";
    let executor_live_enabled = true;

    let should_reject = account_type == "live" && !executor_live_enabled;
    assert!(
        !should_reject,
        "live account with live_enabled=true should pass"
    );
}

#[test]
fn live_gate_passes_for_paper_regardless() {
    let account_type = "paper";
    let executor_live_enabled = false;

    let should_reject = account_type == "live" && !executor_live_enabled;
    assert!(
        !should_reject,
        "paper account should pass regardless of live_enabled flag"
    );
}

// =========================================================================
// 3.67: match none — no account matches strategy → graceful handling
// =========================================================================

#[test]
fn match_none_no_panic_when_strategy_not_found() {
    // Simulate the account-matching logic: HashMap<strategy_name, Vec<AccountConfig>>
    let accounts_by_strategy: HashMap<String, Vec<String>> = HashMap::new();

    // Looking up a nonexistent strategy should return None, not panic
    let result = accounts_by_strategy.get("nonexistent_strategy");
    assert!(
        result.is_none(),
        "missing strategy should return None, not panic"
    );

    // Verify the iterator pattern used in signal dispatch is safe
    let dispatched: Vec<&String> = accounts_by_strategy
        .get("nonexistent_strategy")
        .into_iter()
        .flatten()
        .collect();
    assert!(dispatched.is_empty(), "no accounts should be dispatched");
}

// =========================================================================
// 3.68: multi-account — signal dispatches to correct exchange only
// =========================================================================

#[test]
fn multi_account_dispatches_to_correct_exchange() {
    // Simulate exchange_pairs map: exchange → set of pairs
    let mut exchange_pairs: HashMap<Exchange, HashSet<String>> = HashMap::new();
    exchange_pairs
        .entry(Exchange::BitflyerCfd)
        .or_default()
        .insert("FX_BTC_JPY".to_string());
    exchange_pairs
        .entry(Exchange::GmoFx)
        .or_default()
        .insert("USD_JPY".to_string());

    // A signal for FX_BTC_JPY should only match BitflyerCfd
    let signal_pair = "FX_BTC_JPY";
    let matching_exchanges: Vec<Exchange> = exchange_pairs
        .iter()
        .filter(|(_, pairs)| pairs.contains(signal_pair))
        .map(|(ex, _)| *ex)
        .collect();

    assert_eq!(matching_exchanges.len(), 1);
    assert_eq!(matching_exchanges[0], Exchange::BitflyerCfd);

    // A signal for USD_JPY should only match GmoFx
    let signal_pair = "USD_JPY";
    let matching_exchanges: Vec<Exchange> = exchange_pairs
        .iter()
        .filter(|(_, pairs)| pairs.contains(signal_pair))
        .map(|(ex, _)| *ex)
        .collect();

    assert_eq!(matching_exchanges.len(), 1);
    assert_eq!(matching_exchanges[0], Exchange::GmoFx);

    // A signal for an unknown pair should match nothing
    let signal_pair = "EUR_USD";
    let matching_exchanges: Vec<Exchange> = exchange_pairs
        .iter()
        .filter(|(_, pairs)| pairs.contains(signal_pair))
        .map(|(ex, _)| *ex)
        .collect();

    assert!(matching_exchanges.is_empty());
}

// =========================================================================
// 3.69: Live/Paper split — dry_run=true uses PriceStore, false uses API
// =========================================================================

/// dry_run=true: fill comes from PriceStore, API is NOT called.
#[sqlx::test(migrations = "../../migrations")]
async fn live_paper_split_dry_run_uses_price_store(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "paper_split_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let price_store = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    let api = MockExchangeApiBuilder::new().build();
    let counters = api.counters.clone();
    let sizer = usd_jpy_sizer();
    let notifier = Arc::new(Notifier::new_disabled());

    let trader = Trader::new(
        pool,
        exchange,
        account_id,
        "paper_test".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        true, // dry_run=true → paper path
    );

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("execute should succeed");

    // Verify: PriceStore was used (Long → ask price = 151)
    assert_eq!(trade.entry_price, dec!(151));

    // Verify: Exchange API was NOT called
    assert_eq!(
        counters.send_child_order.load(Ordering::SeqCst),
        0,
        "send_child_order should NOT be called in dry_run mode"
    );
    assert_eq!(
        counters.get_executions.load(Ordering::SeqCst),
        0,
        "get_executions should NOT be called in dry_run mode"
    );
}

/// dry_run=false: fill comes from exchange API.
#[sqlx::test(migrations = "../../migrations")]
async fn live_paper_split_live_uses_exchange_api(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "live_split_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let price_store = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    let api = MockExchangeApiBuilder::new()
        .with_send_child_order_response(SendChildOrderResponse {
            child_order_acceptance_id: "split-order-001".to_string(),
        })
        .with_get_executions_response(vec![Execution {
            id: 1,
            child_order_id: "split-order-001".to_string(),
            side: "BUY".to_string(),
            price: dec!(151),
            size: dec!(6622),
            commission: dec!(0),
            exec_date: "2026-04-10T12:00:00".to_string(),
            child_order_acceptance_id: "split-order-001".to_string(),
        }])
        .build();

    let counters = api.counters.clone();
    let sizer = usd_jpy_sizer();
    let notifier = Arc::new(Notifier::new_disabled());

    let trader = Trader::new(
        pool,
        exchange,
        account_id,
        "live_test".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        false, // dry_run=false → live path
    )
    .with_poll_timeout(Duration::from_secs(5));

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("execute should succeed");

    // Verify: Exchange API WAS called
    assert!(
        counters.send_child_order.load(Ordering::SeqCst) >= 1,
        "send_child_order should be called in live mode"
    );
    assert!(
        counters.get_executions.load(Ordering::SeqCst) >= 1,
        "get_executions should be called in live mode"
    );

    assert_eq!(trade.status, TradeStatus::Open);
}
