//! Phase 3: commission を Trade.fees に累積する経路の paper=live 等価性テスト。
//!
//! 小さな in-test mock `CommissionMockApi` で `Execution.commission` を返し、
//! - live: open / close の commission が `Trade.fees` に積まれる
//! - paper: `commission::estimate_*` が現状 0 を返すので `Trade.fees == 0`
//!
//! を確認する。

use std::collections::HashMap;
use std::sync::Arc;

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
use uuid::Uuid;

struct CommissionMockApi {
    commission: Decimal,
}

#[async_trait]
impl ExchangeApi for CommissionMockApi {
    async fn send_child_order(
        &self,
        _req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        Ok(SendChildOrderResponse {
            child_order_acceptance_id: "mock-order".into(),
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
        Ok(vec![Execution {
            id: 1,
            child_order_id: "mock-order".into(),
            side: "BUY".into(),
            price: dec!(150),
            size: dec!(1000),
            commission: self.commission,
            exec_date: "2026-05-15T10:00:00Z".into(),
            child_order_acceptance_id: "mock-order".into(),
        }])
    }

    async fn get_positions(&self, _product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        Ok(vec![])
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        anyhow::bail!("not used")
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
        Ok(Some("mock-pos-1".into()))
    }

    fn requires_close_position_id(&self) -> bool {
        true
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
    dry_run: bool,
) -> Trader {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let sizer = Arc::new(PositionSizer::new(min_sizes));
    let notifier = Arc::new(Notifier::new_disabled());
    Trader::new(
        pool,
        Exchange::GmoFx,
        account_id,
        "commission_test".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        dry_run,
    )
    .with_poll_timeout(std::time::Duration::from_millis(500))
}

fn make_signal() -> Signal {
    Signal {
        strategy_name: "test_strategy".into(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        stop_loss_pct: dec!(0.02),
        take_profit_pct: Some(dec!(0.04)),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        max_hold_until: None,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_open_accumulates_commission_into_fees(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "comm_open",
        "live",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let api: Arc<dyn ExchangeApi> = Arc::new(CommissionMockApi {
        commission: dec!(123),
    });
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps, false);

    let trade = trader
        .execute(&make_signal())
        .await
        .expect("live open should succeed");
    assert_eq!(
        trade.fees,
        dec!(123),
        "open commission must land in Trade.fees"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_open_uses_estimate_commission_zero(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "comm_paper",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    // mock は live commission を返すが、paper 経路では API を呼ばないので使われない
    let api: Arc<dyn ExchangeApi> = Arc::new(CommissionMockApi {
        commission: dec!(999),
    });
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps, true);

    let trade = trader
        .execute(&make_signal())
        .await
        .expect("paper open should succeed");
    assert_eq!(
        trade.fees,
        Decimal::ZERO,
        "paper open should use commission::estimate_open which currently returns 0 for GMO FX"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_close_accumulates_commission_on_top_of_open(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "comm_close",
        "live",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let api: Arc<dyn ExchangeApi> = Arc::new(CommissionMockApi {
        commission: dec!(50),
    });
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps, false);

    let trade = trader
        .execute(&make_signal())
        .await
        .expect("live open should succeed");
    assert_eq!(trade.fees, dec!(50), "open commission");

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("live close should succeed");
    assert_eq!(
        closed.fees,
        dec!(100),
        "close commission accumulates on top of open: 50 + 50 = 100"
    );
}
