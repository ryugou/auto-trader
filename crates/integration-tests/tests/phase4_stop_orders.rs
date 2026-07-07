//! Phase 4: 取引所側 SL ストップ注文の Trader 配線 (Task 4.5)。
//!
//! 1. live open → `place_stop_order` が丸め済み trigger で呼ばれ、
//!    `trades.stop_order_id` が保存される。
//! 2. アプリ経由 close → `stop_order_status` → `cancel_stop_order` → 成行 close。
//! 3. stop が既に Executed → 新規注文を出さず stop 約定価格で closed。
//! 4. `place_stop_order` が Err → open は成功し stop_order_id NULL + OrderFailed 通知。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Signal, TradeStatus};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
use auto_trader_integration_tests::mocks::slack_webhook::MockSlackWebhook;
use auto_trader_market::bitflyer_private::{Execution, SendChildOrderResponse, Side};
use auto_trader_market::exchange_api::StopOrderStatus;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

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

/// USD_JPY sizer with a 0.01 price tick so trigger rounding is observable.
fn usd_jpy_sizer_with_tick() -> Arc<PositionSizer> {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let mut units = HashMap::new();
    units.insert(Pair::new("USD_JPY"), dec!(0.01));
    Arc::new(PositionSizer::new(min_sizes, Decimal::ZERO).with_price_units(units))
}

fn one_exec(id: &str, price: Decimal, size: Decimal) -> Vec<Execution> {
    vec![Execution {
        id: 1,
        child_order_id: id.to_string(),
        side: "BUY".to_string(),
        price,
        size,
        commission: dec!(0),
        exec_date: "2026-07-07T00:00:00".to_string(),
        child_order_acceptance_id: id.to_string(),
    }]
}

// =========================================================================
// Test 1: live open places the exchange-side stop with a rounded trigger
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn live_open_places_stop_with_rounded_trigger(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "stop_open_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let price_store = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    // Open fill price = 150.335 → Long SL = 150.335 × 0.98 = 147.3283.
    // With tick 0.01, Long SL rounds up (ceil) → 147.33.
    let api = MockExchangeApiBuilder::new()
        .with_send_child_order_response(SendChildOrderResponse {
            child_order_acceptance_id: "open-001".to_string(),
        })
        .with_get_executions_response(one_exec("open-001", dec!(150.335), dec!(1000)))
        .with_place_stop_order_response("gmo-stop-42")
        .build();
    let counters = api.counters.clone();
    let place_calls = api.place_stop_calls.clone();

    let trader = Trader::new(
        pool.clone(),
        exchange,
        account_id,
        "stop_open_test".to_string(),
        api,
        price_store,
        Arc::new(Notifier::new_disabled()),
        usd_jpy_sizer_with_tick(),
        dec!(1.00),
        false, // live
    )
    .with_poll_timeout(Duration::from_secs(5));

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");

    assert_eq!(
        counters.place_stop_order.load(Ordering::SeqCst),
        1,
        "place_stop_order should be called exactly once on live open"
    );
    // Copy out of the recording so the mutex guard isn't held across the await below.
    let call = {
        let calls = place_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        calls[0].clone()
    };
    assert_eq!(
        call.close_side,
        Side::Sell,
        "Long position closes with Sell"
    );
    assert_eq!(call.size, trade.quantity);
    // Trigger is the SL price rounded to the exchange tick (0.01), NOT the raw SL.
    assert_eq!(
        call.trigger_price,
        dec!(147.33),
        "trigger rounded up to tick"
    );
    assert_ne!(
        call.trigger_price, trade.stop_loss,
        "rounded trigger differs from the raw stop_loss price"
    );

    // stop_order_id is persisted on the Trade and in the DB row.
    assert_eq!(trade.stop_order_id.as_deref(), Some("gmo-stop-42"));
    let db_trade = auto_trader_db::trades::get_trade_by_id(&pool, trade.id)
        .await
        .expect("query")
        .expect("trade row exists");
    assert_eq!(db_trade.stop_order_id.as_deref(), Some("gmo-stop-42"));
    assert_eq!(trade.status, TradeStatus::Open);
}

// =========================================================================
// Test 2: close checks status, cancels an Active stop, then market-closes
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn close_cancels_active_stop_then_market_closes(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "stop_close_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let price_store = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    let api = MockExchangeApiBuilder::new()
        .with_send_child_order_response(SendChildOrderResponse {
            child_order_acceptance_id: "ord-001".to_string(),
        })
        .with_get_executions_response(one_exec("ord-001", dec!(150), dec!(1000)))
        .with_place_stop_order_response("gmo-stop-1")
        // default stop_order_status = Active
        .build();
    let counters = api.counters.clone();

    let trader = Trader::new(
        pool.clone(),
        exchange,
        account_id,
        "stop_close_test".to_string(),
        api,
        price_store,
        Arc::new(Notifier::new_disabled()),
        usd_jpy_sizer_with_tick(),
        dec!(1.00),
        false,
    )
    .with_poll_timeout(Duration::from_secs(5));

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");

    counters.send_child_order.store(0, Ordering::SeqCst);
    counters.stop_order_status.store(0, Ordering::SeqCst);
    counters.cancel_stop_order.store(0, Ordering::SeqCst);

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::Manual)
        .await
        .expect("close should succeed");

    assert!(
        counters.stop_order_status.load(Ordering::SeqCst) >= 1,
        "close must check stop status first"
    );
    assert_eq!(
        counters.cancel_stop_order.load(Ordering::SeqCst),
        1,
        "an Active stop must be cancelled before the market close"
    );
    assert!(
        counters.send_child_order.load(Ordering::SeqCst) >= 1,
        "market close order is sent after cancelling the stop"
    );
    assert_eq!(closed.status, TradeStatus::Closed);
    // Market close fill = mock exec price 150.
    assert_eq!(closed.exit_price, Some(dec!(150)));
}

// =========================================================================
// Test 3: an already-Executed stop closes at the stop fill, no new order
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn close_records_executed_stop_without_new_order(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "stop_exec_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let price_store = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    let api = MockExchangeApiBuilder::new()
        .with_send_child_order_response(SendChildOrderResponse {
            child_order_acceptance_id: "ord-002".to_string(),
        })
        .with_get_executions_response(one_exec("ord-002", dec!(150), dec!(1000)))
        .with_place_stop_order_response("gmo-stop-2")
        .with_stop_order_status_response(StopOrderStatus::Executed {
            price: dec!(147.33),
            commission: dec!(5),
        })
        .build();
    let counters = api.counters.clone();

    let trader = Trader::new(
        pool.clone(),
        exchange,
        account_id,
        "stop_exec_test".to_string(),
        api,
        price_store,
        Arc::new(Notifier::new_disabled()),
        usd_jpy_sizer_with_tick(),
        dec!(1.00),
        false,
    )
    .with_poll_timeout(Duration::from_secs(5));

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");

    counters.send_child_order.store(0, Ordering::SeqCst);
    counters.cancel_stop_order.store(0, Ordering::SeqCst);

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit)
        .await
        .expect("close should succeed");

    assert_eq!(
        counters.send_child_order.load(Ordering::SeqCst),
        0,
        "an already-executed stop must NOT trigger a second (market) order"
    );
    assert_eq!(
        counters.cancel_stop_order.load(Ordering::SeqCst),
        0,
        "an executed stop is not cancelled"
    );
    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(
        closed.exit_price,
        Some(dec!(147.33)),
        "exit price is the stop's fill price"
    );
}

// =========================================================================
// Test 4: a failed stop placement does NOT fail the open; alert fires
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn stop_placement_failure_does_not_fail_open_and_alerts(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "stop_fail_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let price_store = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    let (slack, webhook_url) = MockSlackWebhook::start().await;

    let api = MockExchangeApiBuilder::new()
        .with_send_child_order_response(SendChildOrderResponse {
            child_order_acceptance_id: "ord-003".to_string(),
        })
        .with_get_executions_response(one_exec("ord-003", dec!(150), dec!(1000)))
        .with_failures("place_stop_order", 1) // first (only) call fails
        .build();
    let counters = api.counters.clone();

    let trader = Trader::new(
        pool.clone(),
        exchange,
        account_id,
        "stop_fail_test".to_string(),
        api,
        price_store,
        Arc::new(Notifier::new(Some(webhook_url))),
        usd_jpy_sizer_with_tick(),
        dec!(1.00),
        false,
    )
    .with_poll_timeout(Duration::from_secs(5));

    let signal = make_signal("USD_JPY", Direction::Long);
    // Open still succeeds even though the stop placement failed.
    let trade = trader
        .execute(&signal)
        .await
        .expect("open must succeed even if stop placement fails");

    assert_eq!(counters.place_stop_order.load(Ordering::SeqCst), 1);
    assert_eq!(trade.status, TradeStatus::Open);
    assert!(
        trade.stop_order_id.is_none(),
        "stop_order_id stays NULL when placement fails"
    );
    let db_trade = auto_trader_db::trades::get_trade_by_id(&pool, trade.id)
        .await
        .expect("query")
        .expect("trade row exists");
    assert!(db_trade.stop_order_id.is_none());

    // The OrderFailed alert is fired fire-and-forget; poll for it.
    let mut saw_alert = false;
    for _ in 0..40 {
        let bodies = slack.captured_bodies();
        if bodies.iter().any(|b| b.contains("stop order")) {
            saw_alert = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        saw_alert,
        "a stop-placement-failure alert must be sent to Slack. bodies={:?}",
        slack.captured_bodies()
    );
}
