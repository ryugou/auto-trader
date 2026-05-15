//! Phase 3: GMO live close hands off `trade.exchange_position_id`.
//!
//! These tests use a tiny in-test `ExchangeApi` mock that
//!  - reports `requires_close_position_id() = true` (mimics GMO FX),
//!  - captures the `close_position_id` field of every `send_child_order` call.
//!
//! They verify two properties of the executor:
//!  1. When the open trade row has `exchange_position_id = Some(pid)`,
//!     `Trader::close_position` propagates `pid` through to the API as the
//!     request's `close_position_id` — i.e. live closes will hit
//!     `/v1/closeOrder` against the right position.
//!  2. When the trade row has `exchange_position_id = None` on an exchange
//!     that requires it, the close is rejected before dispatch so we never
//!     accidentally open an opposite position via `/v1/order`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::*;
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_market::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Minimal mock that captures the most recent `close_position_id` seen by
/// `send_child_order` and returns canned successful fills.
struct CaptureCloseMockApi {
    requires_close_position_id: bool,
    last_close_position_id: Mutex<Option<String>>,
    send_calls: AtomicUsize,
}

impl CaptureCloseMockApi {
    fn new(requires_close_position_id: bool) -> Self {
        Self {
            requires_close_position_id,
            last_close_position_id: Mutex::new(None),
            send_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ExchangeApi for CaptureCloseMockApi {
    async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        *self.last_close_position_id.lock().await = req.close_position_id.clone();
        self.send_calls.fetch_add(1, Ordering::SeqCst);
        Ok(SendChildOrderResponse {
            child_order_acceptance_id: "mock-close-1".into(),
        })
    }

    async fn get_child_orders(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        Ok(vec![])
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        // Single fill at 150 for size 1000 — sufficient for poll_executions.
        Ok(vec![Execution {
            id: 1,
            child_order_id: "mock-close-1".into(),
            side: "SELL".into(),
            price: dec!(150),
            size: dec!(1000),
            commission: dec!(0),
            exec_date: "2026-05-15T10:00:00Z".into(),
            child_order_acceptance_id: "mock-close-1".into(),
        }])
    }

    async fn get_positions(&self, _product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        Ok(vec![])
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        anyhow::bail!("not used in this test")
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn resolve_position_id(
        &self,
        _product_code: &str,
        _after: chrono::DateTime<chrono::Utc>,
        _expected_side: Side,
        _expected_size: Decimal,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    fn requires_close_position_id(&self) -> bool {
        self.requires_close_position_id
    }
}

async fn make_price_store(exchange: Exchange, pair: &str) -> Arc<PriceStore> {
    let feed_key = FeedKey::new(exchange, Pair::new(pair));
    let store = PriceStore::new(vec![feed_key.clone()]);
    store
        .update(
            feed_key,
            LatestTick {
                price: dec!(150),
                best_bid: Some(dec!(149.9)),
                best_ask: Some(dec!(150.1)),
                ts: Utc::now(),
            },
        )
        .await;
    store
}

fn make_trader(
    pool: sqlx::PgPool,
    account_id: Uuid,
    api: Arc<dyn ExchangeApi>,
    price_store: Arc<PriceStore>,
) -> Trader {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let sizer = Arc::new(PositionSizer::new(min_sizes));
    let notifier = Arc::new(Notifier::new_disabled());
    Trader::new(
        pool,
        Exchange::GmoFx,
        account_id,
        "gmo_test".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        // dry_run = false: exercises the live close path (send_child_order +
        // ensure_close_position_id_present guard).
        false,
    )
    .with_poll_timeout(std::time::Duration::from_millis(500))
}

async fn seed_open_trade(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    exchange_position_id: Option<String>,
) -> Trade {
    let trade = Trade {
        id: Uuid::new_v4(),
        account_id,
        strategy_name: "test_strategy".into(),
        pair: Pair::new("USD_JPY"),
        exchange: Exchange::GmoFx,
        direction: Direction::Long,
        entry_price: dec!(150),
        exit_price: None,
        stop_loss: dec!(147),
        take_profit: Some(dec!(155)),
        quantity: dec!(1000),
        leverage: dec!(25),
        fees: dec!(0),
        entry_at: Utc::now(),
        exit_at: None,
        pnl_amount: None,
        exit_reason: None,
        status: TradeStatus::Open,
        max_hold_until: None,
        exchange_position_id,
    };
    auto_trader_db::trades::insert_trade(pool, &trade)
        .await
        .expect("insert_trade failed");
    // Lock margin so the close path's release_margin has something to release.
    let margin = trade.entry_price * trade.quantity / trade.leverage;
    let margin = margin.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    let mut tx = pool.begin().await.unwrap();
    auto_trader_db::trades::lock_margin(&mut tx, account_id, trade.id, margin)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    trade
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_close_propagates_exchange_position_id_to_api(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "gmo_handoff",
        "live",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let mock = Arc::new(CaptureCloseMockApi::new(true));
    let api: Arc<dyn ExchangeApi> = mock.clone();
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps);

    let trade = seed_open_trade(&pool, account_id, Some("gmo-pos-42".to_string())).await;

    trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed when exchange_position_id is present");

    assert_eq!(mock.send_calls.load(Ordering::SeqCst), 1);
    let captured = mock.last_close_position_id.lock().await.clone();
    assert_eq!(
        captured,
        Some("gmo-pos-42".into()),
        "close_position must propagate Trade.exchange_position_id as close_position_id"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_close_refuses_when_required_position_id_is_missing(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "gmo_missing_pid",
        "live",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let mock = Arc::new(CaptureCloseMockApi::new(true));
    let api: Arc<dyn ExchangeApi> = mock.clone();
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps);

    let trade = seed_open_trade(&pool, account_id, None).await;

    let err = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit)
        .await
        .expect_err("close must refuse when exchange requires position id but trade has none");
    let msg = err.to_string();
    assert!(
        msg.contains("exchange_position_id") || msg.contains("position id"),
        "error should explain why: {msg}"
    );
    assert_eq!(
        mock.send_calls.load(Ordering::SeqCst),
        0,
        "no API send must occur when the trade lacks the required position id"
    );
}
