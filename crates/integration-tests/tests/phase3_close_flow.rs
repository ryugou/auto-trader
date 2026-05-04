//! Phase 3: Close flow — CAS lock, trade status transitions, concurrent close.
//!
//! DB + Trader レベルのクローズフローテスト。
//!
//! SKIP: 3.72 (Phase 2 failure → lock release) — requires mock DB failure injection
//! SKIP: 3.73 (Phase 3 failure → notification) — requires mock DB failure injection

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Signal, TradeStatus};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::helpers::seed;
use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
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

    Trader::new(
        pool,
        exchange,
        account_id,
        "test_close".to_string(),
        api,
        price_store,
        notifier,
        sizer,
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

    // Open and close a trade
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");
    assert_eq!(closed.status, TradeStatus::Closed);

    // Try to close again — should fail
    let result = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit)
        .await;
    assert!(
        result.is_err(),
        "re-closing a closed trade should fail"
    );
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

    // Open a trade
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader1.execute(&signal).await.expect("open should succeed");
    let trade_id = trade.id.to_string();

    // Spawn two concurrent close calls
    let id1 = trade_id.clone();
    let handle1 = tokio::spawn(async move {
        trader1.close_position(&id1, ExitReason::TpHit).await
    });
    let id2 = trade_id;
    let handle2 = tokio::spawn(async move {
        trader2.close_position(&id2, ExitReason::SlHit).await
    });

    let (r1, r2) = tokio::join!(handle1, handle2);
    let result1 = r1.expect("task 1 should not panic");
    let result2 = r2.expect("task 2 should not panic");

    // Exactly one should succeed and one should fail
    let successes = [&result1, &result2]
        .iter()
        .filter(|r| r.is_ok())
        .count();
    let failures = [&result1, &result2]
        .iter()
        .filter(|r| r.is_err())
        .count();

    assert_eq!(successes, 1, "exactly one close should succeed");
    assert_eq!(failures, 1, "exactly one close should fail");
}
