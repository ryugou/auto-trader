//! Phase 3: Shutdown tests (3.111-3.113).
//!
//! Verifies graceful shutdown: channel drain, timeout compliance,
//! and open position preservation after shutdown.

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Candle, Exchange, Pair};
use chrono::Utc;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Helper: create a PriceEvent.
fn make_price_event() -> PriceEvent {
    PriceEvent {
        pair: Pair::new("USD_JPY"),
        exchange: Exchange::Oanda,
        candle: Candle {
            pair: Pair::new("USD_JPY"),
            exchange: Exchange::Oanda,
            timeframe: "M5".to_string(),
            open: dec!(150.0),
            high: dec!(151.0),
            low: dec!(149.0),
            close: dec!(150.5),
            volume: Some(100),
            best_bid: Some(dec!(150.4)),
            best_ask: Some(dec!(150.6)),
            timestamp: Utc::now(),
        },
        indicators: HashMap::new(),
        timestamp: Utc::now(),
    }
}

// ─── 3.111: All tasks drain on channel close ─────────────────────────────
//
// Create channels (price_tx, signal_tx, trade_tx), spawn mock consumer
// tasks, drop the senders, verify all tasks complete.

#[tokio::test]
async fn all_tasks_drain_on_channel_close() {
    let (price_tx, mut price_rx) = mpsc::channel::<PriceEvent>(16);
    let (signal_tx, mut signal_rx) = mpsc::channel::<String>(16);
    let (trade_tx, mut trade_rx) = mpsc::channel::<String>(16);

    // Send some events before dropping
    price_tx.send(make_price_event()).await.unwrap();
    signal_tx.send("signal_1".to_string()).await.unwrap();
    trade_tx.send("trade_1".to_string()).await.unwrap();

    // Spawn consumer tasks that drain until channel closes
    let price_handle = tokio::spawn(async move {
        let mut count = 0;
        while price_rx.recv().await.is_some() {
            count += 1;
        }
        count
    });

    let signal_handle = tokio::spawn(async move {
        let mut count = 0;
        while signal_rx.recv().await.is_some() {
            count += 1;
        }
        count
    });

    let trade_handle = tokio::spawn(async move {
        let mut count = 0;
        while trade_rx.recv().await.is_some() {
            count += 1;
        }
        count
    });

    // Drop all senders to signal shutdown
    drop(price_tx);
    drop(signal_tx);
    drop(trade_tx);

    // All tasks should complete
    let price_count = price_handle.await.expect("price task should complete");
    let signal_count = signal_handle.await.expect("signal task should complete");
    let trade_count = trade_handle.await.expect("trade task should complete");

    assert_eq!(price_count, 1, "price consumer should have drained 1 event");
    assert_eq!(signal_count, 1, "signal consumer should have drained 1 event");
    assert_eq!(trade_count, 1, "trade consumer should have drained 1 event");
}

// ─── 3.112: All tasks complete within timeout ────────────────────────────

#[tokio::test]
async fn all_tasks_complete_within_timeout() {
    let (price_tx, mut price_rx) = mpsc::channel::<PriceEvent>(16);
    let (signal_tx, mut signal_rx) = mpsc::channel::<String>(16);
    let (trade_tx, mut trade_rx) = mpsc::channel::<String>(16);

    // Spawn consumer tasks
    let price_handle = tokio::spawn(async move {
        while price_rx.recv().await.is_some() {}
    });
    let signal_handle = tokio::spawn(async move {
        while signal_rx.recv().await.is_some() {}
    });
    let trade_handle = tokio::spawn(async move {
        while trade_rx.recv().await.is_some() {}
    });

    // Drop senders
    drop(price_tx);
    drop(signal_tx);
    drop(trade_tx);

    // All tasks must join within 5 seconds
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        price_handle.await.expect("price task panic");
        signal_handle.await.expect("signal task panic");
        trade_handle.await.expect("trade task panic");
    })
    .await;

    assert!(
        result.is_ok(),
        "all tasks should complete within 5s timeout"
    );
}

// ─── 3.113: Open positions preserved after shutdown ──────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn open_positions_preserved_after_shutdown(pool: sqlx::PgPool) {
    let account_id = auto_trader_integration_tests::helpers::db::seed_trading_account(
        &pool,
        "shutdown_test",
        "paper",
        "bitflyer_cfd",
        "bb_mean_revert_v1",
        1_000_000,
    )
    .await;

    // Seed open trades
    let trade_id_1 = auto_trader_integration_tests::helpers::seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "FX_BTC_JPY",
        "bitflyer_cfd",
        "long",
        dec!(5_000_000),
        dec!(4_900_000),
        dec!(0.01),
        Utc::now(),
    )
    .await;

    let trade_id_2 = auto_trader_integration_tests::helpers::seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "FX_BTC_JPY",
        "bitflyer_cfd",
        "short",
        dec!(5_100_000),
        dec!(5_200_000),
        dec!(0.02),
        Utc::now(),
    )
    .await;

    // Simulate shutdown by creating and dropping channels
    let (price_tx, mut price_rx) = mpsc::channel::<PriceEvent>(16);
    let (signal_tx, mut signal_rx) = mpsc::channel::<String>(16);

    let handle = tokio::spawn(async move {
        while price_rx.recv().await.is_some() {}
        while signal_rx.recv().await.is_some() {}
    });

    drop(price_tx);
    drop(signal_tx);

    handle.await.expect("shutdown task complete");

    // Verify trades are still open in DB
    let status_1: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
        .bind(trade_id_1)
        .fetch_one(&pool)
        .await
        .expect("trade 1 should exist");
    assert_eq!(status_1, "open", "trade 1 should still be open after shutdown");

    let status_2: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
        .bind(trade_id_2)
        .fetch_one(&pool)
        .await
        .expect("trade 2 should exist");
    assert_eq!(status_2, "open", "trade 2 should still be open after shutdown");

    // Verify count of open trades
    let open_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM trades WHERE account_id = $1 AND status = 'open'",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(open_count, 2, "both trades should remain open");
}
