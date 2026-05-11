//! Phase 3: Close flow — CAS lock, trade status transitions, concurrent close.
//!
//! DB + Trader レベルのクローズフローテスト。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Signal, TradeStatus};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::{read_current_balance, seed_trading_account};
use auto_trader_integration_tests::helpers::seed;
use auto_trader_integration_tests::helpers::sizing_invariants;
use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
use auto_trader_integration_tests::mocks::slack_webhook::MockSlackWebhook;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::Duration;
use uuid::Uuid;

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

fn make_trader(
    pool: sqlx::PgPool,
    exchange: Exchange,
    account_id: Uuid,
    price_store: Arc<PriceStore>,
) -> Trader {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let sizer = Arc::new(PositionSizer::new(min_sizes));
    let api = MockExchangeApiBuilder::new().build();
    let notifier = Arc::new(Notifier::new_disabled());

    // All call sites for `make_trader` in this file pass `Exchange::GmoFx`, so the
    // hardcoded `dec!(1.00)` matches the production `[exchange_margin.gmo_fx]` value.
    Trader::new(
        pool,
        exchange,
        account_id,
        "test_close".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        true,
    )
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

// =========================================================================
// 3.71: CAS lock — status=closing のトレードは close_position で失敗
// =========================================================================

/// status='closing' のトレードに対する close_position は失敗する。
/// acquire_close_lock は status='open' を要求するため。
#[sqlx::test(migrations = "../../migrations")]
async fn cas_lock_rejects_closing_trade(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "cas_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    // Seed an open trade, then manually set it to 'closing'
    let trade_id = seed::seed_open_trade(
        &pool,
        account_id,
        "test_strategy",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(149),
        dec!(1),
        Utc::now(),
    )
    .await;

    // Manually transition to 'closing'
    sqlx::query("UPDATE trades SET status = 'closing', closing_started_at = NOW() WHERE id = $1")
        .bind(trade_id)
        .execute(&pool)
        .await
        .expect("update should succeed");

    // Now try to close via Trader — should fail because acquire_close_lock
    // requires 'open' (or stale 'closing' older than 5 minutes)
    let ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool, exchange, account_id, ps);

    let result = trader
        .close_position(&trade_id.to_string(), ExitReason::SlHit)
        .await;

    assert!(
        result.is_err(),
        "close_position should fail on already-closing trade"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not in 'open' state"),
        "error should indicate non-open state: {err_msg}"
    );
}

// =========================================================================
// 3.74: Trade status transitions — closed trade can't be re-opened
// =========================================================================

/// closed trade に対する close_position は失敗する（既にクローズ済み）。
#[sqlx::test(migrations = "../../migrations")]
async fn closed_trade_cannot_be_closed_again(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "status_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, ps);

    let balance_before_open = read_current_balance(&pool, account_id).await;
    // Open and close a trade
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    // qty: balance=1_000_000, lev=2, Y=1.00, SL=0.02, alloc=1.0, entry=151 (Long@ask), min_lot=1
    //      max_alloc = 1/1.04, raw = 1_000_000 × 2 × (1/1.04) / 151 ≈ 12735.39 → 12735
    assert_eq!(
        trade.quantity,
        dec!(12735),
        "sizer: 1M × 2 × (1/1.04) / 151 → 12735"
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

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");
    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(
        closed.quantity,
        dec!(12735),
        "close should not mutate quantity"
    );
    // Close-side enrichment. Long close at bid=150, entry was ask=151 → loss.
    assert_eq!(closed.exit_reason, Some(ExitReason::TpHit));
    let exit_price = closed.exit_price.expect("exit_price must be set");
    assert_eq!(exit_price, dec!(150), "Long close fills at bid=150");
    let expected_pnl = sizing_invariants::expected_pnl(
        closed.entry_price,
        exit_price,
        closed.quantity,
        closed.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(closed.pnl_amount, Some(expected_pnl));
    let balance_after_close = read_current_balance(&pool, account_id).await;
    assert_eq!(
        balance_after_close,
        balance_before_open + expected_pnl - closed.fees,
    );

    // Try to close again — should fail
    let result = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit)
        .await;
    assert!(result.is_err(), "re-closing a closed trade should fail");
}

// =========================================================================
// 3.49: Concurrent close — 2 concurrent close_position on same trade
// =========================================================================

/// 同一トレードに対する 2 つの並行 close_position — 1 つだけ成功。
#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_close_only_one_succeeds(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "concurrent_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader1 = make_trader(pool.clone(), exchange, account_id, ps.clone());
    let trader2 = make_trader(pool.clone(), exchange, account_id, ps);

    let balance_before_open = read_current_balance(&pool, account_id).await;
    // Open a trade
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader1.execute(&signal).await.expect("open should succeed");
    // qty: 1M × 2 × (1/1.04) / 151 → 12735 (Long@ask=151)
    assert_eq!(
        trade.quantity,
        dec!(12735),
        "sizer: 1M × 2 × (1/1.04) / 151 → 12735"
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
    let trade_id = trade.id.to_string();

    // Spawn two concurrent close calls
    let id1 = trade_id.clone();
    let handle1 =
        tokio::spawn(async move { trader1.close_position(&id1, ExitReason::TpHit).await });
    let id2 = trade_id;
    let handle2 =
        tokio::spawn(async move { trader2.close_position(&id2, ExitReason::SlHit).await });

    let (r1, r2) = tokio::join!(handle1, handle2);
    let result1 = r1.expect("task 1 should not panic");
    let result2 = r2.expect("task 2 should not panic");

    // Exactly one should succeed and one should fail
    let successes = [&result1, &result2].iter().filter(|r| r.is_ok()).count();
    let failures = [&result1, &result2].iter().filter(|r| r.is_err()).count();

    assert_eq!(successes, 1, "exactly one close should succeed");
    assert_eq!(failures, 1, "exactly one close should fail");
}

// =========================================================================
// 3.72: Phase 2 failure → lock release
// =========================================================================

/// Phase 2 failure (send_child_order fails) → trade status returns to 'open'.
///
/// When fill_close fails (e.g. exchange API error), the close_position method
/// should release the CAS lock, reverting the trade from 'closing' back to 'open'.
#[sqlx::test(migrations = "../../migrations")]
async fn phase2_failure_releases_lock(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "phase2_fail_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    // First: create a trader with working API to open a trade
    let open_api = MockExchangeApiBuilder::new().build();
    let sizer = Arc::new(PositionSizer::new({
        let mut m = HashMap::new();
        m.insert(Pair::new("USD_JPY"), dec!(1));
        m
    }));
    let notifier = Arc::new(Notifier::new_disabled());

    let open_trader = Trader::new(
        pool.clone(),
        exchange,
        account_id,
        "phase2_fail_test".to_string(),
        open_api,
        ps.clone(),
        notifier.clone(),
        sizer.clone(),
        dec!(1.00),
        true, // dry_run to open easily
    );

    let balance_before_open = read_current_balance(&pool, account_id).await;
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = open_trader
        .execute(&signal)
        .await
        .expect("open should succeed");
    // qty: dry_run open — 1M × 2 × (1/1.04) / 151 → 12735 (Long@ask=151)
    assert_eq!(
        trade.quantity,
        dec!(12735),
        "sizer: 1M × 2 × (1/1.04) / 151 → 12735"
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
    let trade_id = trade.id;

    // Now create a trader with a FAILING API (dry_run=false) for the close attempt
    let fail_api = MockExchangeApiBuilder::new()
        .with_failures("send_child_order", 10) // fail many times
        .build();

    let close_trader = Trader::new(
        pool.clone(),
        exchange,
        account_id,
        "phase2_fail_test".to_string(),
        fail_api,
        ps,
        notifier,
        sizer,
        dec!(1.00),
        false, // dry_run=false → live path that calls send_child_order
    )
    .with_poll_timeout(Duration::from_millis(100));

    // Attempt close — should fail because send_child_order errors
    let result = close_trader
        .close_position(&trade_id.to_string(), ExitReason::SlHit)
        .await;

    assert!(result.is_err(), "close should fail due to API error");

    // Verify the trade status was reverted to 'open' (lock released)
    let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_one(&pool)
        .await
        .expect("query should succeed");

    assert_eq!(
        status, "open",
        "Phase 2 failure should release lock, reverting status to 'open'"
    );
}

// =========================================================================
// 3.73: Phase 3 → notification on successful close
// =========================================================================

/// After a successful close_position, verify a notification was created in the DB.
/// (We test the notification path exists by checking the notifications table.)
#[sqlx::test(migrations = "../../migrations")]
async fn close_position_creates_notification(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "notify_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, ps);

    let balance_before_open = read_current_balance(&pool, account_id).await;
    // Open a trade
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    // qty: 1M × 2 × (1/1.04) / 151 → 12735 (Long@ask=151)
    assert_eq!(
        trade.quantity,
        dec!(12735),
        "sizer: 1M × 2 × (1/1.04) / 151 → 12735"
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

    // Verify 'trade_opened' notification was created
    let open_notif_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE trade_id = $1 AND kind = 'trade_opened'",
    )
    .bind(trade.id)
    .fetch_one(&pool)
    .await
    .expect("query should succeed");
    assert_eq!(
        open_notif_count, 1,
        "trade_opened notification should exist"
    );

    // Close the trade
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");
    // Close-side enrichment.
    assert_eq!(closed.exit_reason, Some(ExitReason::TpHit));
    let exit_price = closed.exit_price.expect("exit_price must be set");
    assert_eq!(exit_price, dec!(150), "Long close fills at bid=150");
    let expected_pnl = sizing_invariants::expected_pnl(
        closed.entry_price,
        exit_price,
        closed.quantity,
        closed.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(closed.pnl_amount, Some(expected_pnl));
    let balance_after_close = read_current_balance(&pool, account_id).await;
    assert_eq!(
        balance_after_close,
        balance_before_open + expected_pnl - closed.fees,
    );

    // Verify 'trade_closed' notification was created
    let close_notif_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE trade_id = $1 AND kind = 'trade_closed'",
    )
    .bind(closed.id)
    .fetch_one(&pool)
    .await
    .expect("query should succeed");
    assert_eq!(
        close_notif_count, 1,
        "trade_closed notification should exist after close_position"
    );
}

/// Verify that close_position fires a Slack notification via the webhook.
#[sqlx::test(migrations = "../../migrations")]
async fn close_position_sends_slack_notification(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "slack_notify_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;

    // Create a trader with a real webhook mock
    let (slack, webhook_url) = MockSlackWebhook::start().await;
    let notifier = Arc::new(Notifier::new(Some(webhook_url)));

    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let sizer = Arc::new(PositionSizer::new(min_sizes));
    let api = MockExchangeApiBuilder::new().build();

    let trader = Trader::new(
        pool.clone(),
        exchange,
        account_id,
        "slack_notify_test".to_string(),
        api,
        ps,
        notifier,
        sizer,
        dec!(1.00),
        true, // dry_run
    );

    let balance_before_open = read_current_balance(&pool, account_id).await;
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    // qty: 1M × 2 × (1/1.04) / 151 → 12735 (Long@ask=151)
    assert_eq!(
        trade.quantity,
        dec!(12735),
        "sizer: 1M × 2 × (1/1.04) / 151 → 12735"
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

    // Wait briefly for fire-and-forget notification to be sent
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");
    // Close-side enrichment.
    assert_eq!(closed.exit_reason, Some(ExitReason::TpHit));
    let exit_price = closed.exit_price.expect("exit_price must be set");
    assert_eq!(exit_price, dec!(150), "Long close fills at bid=150");
    let expected_pnl = sizing_invariants::expected_pnl(
        closed.entry_price,
        exit_price,
        closed.quantity,
        closed.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(closed.pnl_amount, Some(expected_pnl));

    // Wait for fire-and-forget close notification
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let bodies = slack.captured_bodies();
    // At least 2 notifications: one for open, one for close
    assert!(
        bodies.len() >= 2,
        "should have at least 2 Slack notifications (open + close), got {}",
        bodies.len()
    );
}
