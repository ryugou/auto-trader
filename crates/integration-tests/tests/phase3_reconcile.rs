//! Phase 3: Startup reconciliation integration tests.
//!
//! Tests the `startup_reconcile` module using MockExchangeApi + real DB.
//!
//! 3.75 noop — DB open + exchange has matching position → stays open.
//! 3.76 orphan — DB open + exchange empty → force-closed.
//! 3.77 stale closing — DB closing + exchange has position → reset to open.
//! 3.78 phase3 incomplete — DB closing + exchange empty → set to closed.
//! 3.79 API retry exhaustion — MockExchangeApi fails 3 times → bail.
//! 3.80 API error — MockExchangeApi fails immediately → bail.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use auto_trader_core::types::Exchange;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::helpers::seed;
use auto_trader_market::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse,
};
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::PriceStore;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use uuid::Uuid;

// =========================================================================
// MockExchangeApi for reconciler tests (only get_positions needed)
// =========================================================================

struct ReconcileMockApi {
    positions: HashMap<String, Vec<ExchangePosition>>,
    get_positions_failures_remaining: AtomicU32,
}

impl ReconcileMockApi {
    fn new(positions: HashMap<String, Vec<ExchangePosition>>, failures: u32) -> Arc<Self> {
        Arc::new(Self {
            positions,
            get_positions_failures_remaining: AtomicU32::new(failures),
        })
    }
}

#[async_trait]
impl ExchangeApi for ReconcileMockApi {
    async fn get_positions(&self, product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        if self
            .get_positions_failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                if v > 0 { Some(v - 1) } else { None }
            })
            .is_ok()
        {
            anyhow::bail!("mock get_positions failure");
        }
        Ok(self
            .positions
            .get(product_code)
            .cloned()
            .unwrap_or_default())
    }

    async fn send_child_order(
        &self,
        _req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        unimplemented!("not used in reconciler tests")
    }

    async fn get_child_orders(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        unimplemented!("not used in reconciler tests")
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        unimplemented!("not used in reconciler tests")
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        unimplemented!("not used in reconciler tests")
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        unimplemented!("not used in reconciler tests")
    }

    async fn resolve_position_id(
        &self,
        _product_code: &str,
        _after: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

// =========================================================================
// Helpers
// =========================================================================

fn make_exchange_position(side: &str, size: Decimal) -> ExchangePosition {
    ExchangePosition {
        product_code: "FX_BTC_JPY".to_string(),
        side: side.to_string(),
        price: dec!(11_500_000),
        size,
        commission: dec!(0),
        swap_point_accumulate: dec!(0),
        require_collateral: dec!(0),
        open_date: "2026-01-01T00:00:00".to_string(),
        leverage: dec!(2),
        pnl: dec!(0),
        sfd: dec!(0),
    }
}

fn build_apis(api: Arc<dyn ExchangeApi>) -> HashMap<Exchange, Arc<dyn ExchangeApi>> {
    let mut m = HashMap::new();
    m.insert(Exchange::BitflyerCfd, api);
    m
}

fn empty_price_store() -> Arc<PriceStore> {
    PriceStore::new(vec![])
}

async fn seed_live_account(pool: &PgPool) -> Uuid {
    seed_trading_account(
        pool,
        "reconcile_test",
        "live",
        "bitflyer_cfd",
        "bb_mean_revert_v1",
        30_000,
    )
    .await
}

async fn seed_open_trade_btc(pool: &PgPool, account_id: Uuid) -> Uuid {
    seed::seed_open_trade(
        pool,
        account_id,
        "bb_mean_revert_v1",
        "FX_BTC_JPY",
        "bitflyer_cfd",
        "long",
        dec!(11_500_000),
        dec!(11_155_000),
        dec!(0.001),
        Utc::now(),
    )
    .await
}

async fn seed_closing_trade_btc(pool: &PgPool, account_id: Uuid) -> Uuid {
    let trade_id = seed_open_trade_btc(pool, account_id).await;
    sqlx::query("UPDATE trades SET status = 'closing', closing_started_at = NOW() WHERE id = $1")
        .bind(trade_id)
        .execute(pool)
        .await
        .expect("transition to closing should succeed");
    trade_id
}

// =========================================================================
// 3.75: noop — DB=open, exchange has matching position → stays open
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn reconcile_noop_consistent_open(pool: PgPool) {
    let account_id = seed_live_account(&pool).await;
    let trade_id = seed_open_trade_btc(&pool, account_id).await;

    let mut positions = HashMap::new();
    positions.insert(
        "FX_BTC_JPY".to_string(),
        vec![make_exchange_position("BUY", dec!(0.001))],
    );
    let api = ReconcileMockApi::new(positions, 0);
    let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
        .await
        .unwrap()
        .unwrap();
    let apis = build_apis(api);
    let price_store = empty_price_store();

    auto_trader::startup_reconcile::reconcile_live_accounts_at_startup(
        &pool,
        &[account],
        &apis,
        price_store,
    )
    .await
    .expect("reconcile should succeed");

    let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "open", "consistent trade should remain open");
}

// =========================================================================
// 3.76: orphan — DB=open, exchange empty → force-closed
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn reconcile_orphan_force_closes(pool: PgPool) {
    let account_id = seed_live_account(&pool).await;
    let trade_id = seed_open_trade_btc(&pool, account_id).await;

    // Exchange returns no positions
    let api = ReconcileMockApi::new(HashMap::new(), 0);
    let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
        .await
        .unwrap()
        .unwrap();
    let apis = build_apis(api);
    let price_store = empty_price_store();

    auto_trader::startup_reconcile::reconcile_live_accounts_at_startup(
        &pool,
        &[account],
        &apis,
        price_store,
    )
    .await
    .expect("reconcile should succeed");

    let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "closed", "orphan trade should be force-closed");

    let exit_reason: String = sqlx::query_scalar("SELECT exit_reason FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(exit_reason, "reconciled");
}

// =========================================================================
// 3.77: stale closing — DB=closing, exchange has position → reset to open
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn reconcile_stale_closing_resets_to_open(pool: PgPool) {
    let account_id = seed_live_account(&pool).await;
    let trade_id = seed_closing_trade_btc(&pool, account_id).await;

    let mut positions = HashMap::new();
    positions.insert(
        "FX_BTC_JPY".to_string(),
        vec![make_exchange_position("BUY", dec!(0.001))],
    );
    let api = ReconcileMockApi::new(positions, 0);
    let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
        .await
        .unwrap()
        .unwrap();
    let apis = build_apis(api);
    let price_store = empty_price_store();

    auto_trader::startup_reconcile::reconcile_live_accounts_at_startup(
        &pool,
        &[account],
        &apis,
        price_store,
    )
    .await
    .expect("reconcile should succeed");

    let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        status, "open",
        "stale-closing trade with exchange position should be reset to open"
    );
}

// =========================================================================
// 3.78: phase3 incomplete — DB=closing, exchange empty → set to closed
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn reconcile_phase3_incomplete_force_closes(pool: PgPool) {
    let account_id = seed_live_account(&pool).await;
    let trade_id = seed_closing_trade_btc(&pool, account_id).await;

    // Exchange returns no positions — Phase 2 completed before crash
    let api = ReconcileMockApi::new(HashMap::new(), 0);
    let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
        .await
        .unwrap()
        .unwrap();
    let apis = build_apis(api);
    let price_store = empty_price_store();

    auto_trader::startup_reconcile::reconcile_live_accounts_at_startup(
        &pool,
        &[account],
        &apis,
        price_store,
    )
    .await
    .expect("reconcile should succeed");

    let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        status, "closed",
        "closing trade with no exchange position should be force-closed"
    );

    let exit_reason: String = sqlx::query_scalar("SELECT exit_reason FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(exit_reason, "reconciled");
}

// =========================================================================
// 3.79: API retry exhaustion — get_positions fails all 3 attempts → bail
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn reconcile_api_retry_exhaustion(pool: PgPool) {
    let account_id = seed_live_account(&pool).await;
    let _trade_id = seed_open_trade_btc(&pool, account_id).await;

    // Fail all 3 attempts (MAX_ATTEMPTS = 3)
    let api = ReconcileMockApi::new(HashMap::new(), 3);
    let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
        .await
        .unwrap()
        .unwrap();

    // Pause time so retry sleeps (2s→4s) auto-advance
    tokio::time::pause();

    let apis = build_apis(api);
    let price_store = empty_price_store();

    let result = auto_trader::startup_reconcile::reconcile_live_accounts_at_startup(
        &pool,
        &[account],
        &apis,
        price_store,
    )
    .await;

    assert!(
        result.is_err(),
        "reconcile should fail when get_positions exhausts retries"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("exhausted") || err_msg.contains("retry") || err_msg.contains("retries"),
        "error should mention retry exhaustion: {err_msg}"
    );
}

// =========================================================================
// 3.80: API error — get_positions fails immediately → bail
// =========================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn reconcile_api_immediate_error(pool: PgPool) {
    let account_id = seed_live_account(&pool).await;
    let _trade_id = seed_open_trade_btc(&pool, account_id).await;

    // Fail all 3 attempts immediately
    let api = ReconcileMockApi::new(HashMap::new(), 3);
    let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
        .await
        .unwrap()
        .unwrap();

    tokio::time::pause();

    let apis = build_apis(api);
    let price_store = empty_price_store();

    let result = auto_trader::startup_reconcile::reconcile_live_accounts_at_startup(
        &pool,
        &[account],
        &apis,
        price_store,
    )
    .await;

    assert!(result.is_err(), "reconcile should bail on API error");
}
