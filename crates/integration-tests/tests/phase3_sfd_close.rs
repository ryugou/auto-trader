//! Phase 3: SFD (Swap For Difference) を Trade.fees に積算する経路の
//! paper=live 等価性テスト。
//!
//! 小さな in-test mock `SfdMockApi` で:
//!   - send_child_order/get_child_orders/get_executions: open/close 用の
//!     固定 execution + commission を返す
//!   - fetch_close_sfd: テストごとに指定した値 (もしくは Err)
//!
//! を返し、以下を確認:
//!   - live + sfd>0  : close 時に fees に open+close commission + sfd が積まれ DB にも反映
//!   - live + sfd=0  : fees に SFD 加算なし、DB も一致
//!   - paper         : sfd::estimate=0 経路で fees 不変
//!   - sfd fetch err : warn + sfd=0 で close 自体は成功 (close をブロックしない)

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::*;
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_market::bitflyer_private::{
    ChildOrder, ChildOrderState, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

// === Mock API =============================================================

struct SfdMockApi {
    fee_commission: Decimal,
    sfd: Result<Decimal, &'static str>,
}

#[async_trait]
impl ExchangeApi for SfdMockApi {
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
        _acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        Ok(vec![ChildOrder {
            id: 1,
            child_order_id: "ORD-1".into(),
            product_code: "FX_BTC_JPY".into(),
            side: "BUY".into(),
            child_order_type: "MARKET".into(),
            price: dec!(36000),
            average_price: dec!(36000),
            size: dec!(0.01),
            child_order_state: ChildOrderState::Completed,
            expire_date: "2099-01-01T00:00:00".into(),
            child_order_date: "2026-05-17T00:00:00".into(),
            child_order_acceptance_id: "mock-order".into(),
            outstanding_size: dec!(0),
            cancel_size: dec!(0),
            executed_size: dec!(0.01),
            total_commission: self.fee_commission,
        }])
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        _acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        Ok(vec![Execution {
            id: 1,
            child_order_id: "ORD-1".into(),
            side: "BUY".into(),
            price: dec!(36000),
            size: dec!(0.01),
            commission: self.fee_commission,
            exec_date: "2026-05-17T00:00:00Z".into(),
            child_order_acceptance_id: "mock-order".into(),
        }])
    }

    async fn get_positions(&self, _product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        Ok(vec![])
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        anyhow::bail!("not used")
    }

    async fn cancel_child_order(&self, _: &str, _: &str) -> anyhow::Result<()> {
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

    async fn fetch_close_sfd(&self, _product_code: &str) -> anyhow::Result<Decimal> {
        match self.sfd {
            Ok(v) => Ok(v),
            Err(msg) => Err(anyhow!(msg)),
        }
    }
}

// === Fixtures =============================================================

async fn make_price_store(exchange: Exchange, pair: &str) -> Arc<PriceStore> {
    let feed_key = FeedKey::new(exchange, Pair::new(pair));
    let store = PriceStore::new(vec![feed_key.clone()]);
    store
        .update(
            feed_key,
            LatestTick {
                price: dec!(36000),
                best_bid: Some(dec!(35999)),
                best_ask: Some(dec!(36001)),
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
    min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.01));
    let sizer = Arc::new(PositionSizer::new(min_sizes));
    let notifier = Arc::new(Notifier::new_disabled());
    Trader::new(
        pool,
        Exchange::BitflyerCfd,
        account_id,
        "sfd_test".to_string(),
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
        strategy_name: "sfd_test".into(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        stop_loss_pct: dec!(0.02),
        take_profit_pct: Some(dec!(0.04)),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(0.1),
        max_hold_until: None,
    }
}

// === Tests ================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn live_close_accumulates_sfd_into_fees(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "sfd_live",
        "live",
        "bitflyer_cfd",
        "sfd_test",
        1_000_000,
    )
    .await;
    let api: Arc<dyn ExchangeApi> = Arc::new(SfdMockApi {
        fee_commission: dec!(10),
        sfd: Ok(dec!(100)),
    });
    let ps = make_price_store(Exchange::BitflyerCfd, "FX_BTC_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps, false);

    let trade = trader.execute(&make_signal()).await.expect("open succeeds");
    assert_eq!(trade.fees, dec!(10), "open commission");

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close succeeds");
    assert_eq!(
        closed.fees,
        dec!(120),
        "open=10 + close commission=10 + sfd=100"
    );

    let from_db = auto_trader_db::trades::get_trade_by_id(&pool, closed.id)
        .await
        .expect("db query")
        .expect("row exists");
    assert_eq!(
        from_db.fees,
        dec!(120),
        "DB must persist sfd-inclusive fees (regression guard for trader.rs:update_trade_closed bug fix)"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_close_with_zero_sfd_leaves_fees_unchanged(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "sfd_live_zero",
        "live",
        "bitflyer_cfd",
        "sfd_test",
        1_000_000,
    )
    .await;
    let api: Arc<dyn ExchangeApi> = Arc::new(SfdMockApi {
        fee_commission: dec!(5),
        sfd: Ok(Decimal::ZERO),
    });
    let ps = make_price_store(Exchange::BitflyerCfd, "FX_BTC_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps, false);

    let trade = trader.execute(&make_signal()).await.expect("open");
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close");
    assert_eq!(closed.fees, dec!(10), "open + close commission, no sfd");
    let from_db = auto_trader_db::trades::get_trade_by_id(&pool, closed.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(from_db.fees, dec!(10));
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_close_uses_sfd_estimate_zero(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "sfd_paper",
        "paper",
        "bitflyer_cfd",
        "sfd_test",
        1_000_000,
    )
    .await;
    // paper 経路では fetch_close_sfd は呼ばれない (estimate=0 が使われる)。
    // mock の sfd=999 を返したとしても無視されることを assert で確認。
    let api: Arc<dyn ExchangeApi> = Arc::new(SfdMockApi {
        fee_commission: dec!(999),
        sfd: Ok(dec!(999)),
    });
    let ps = make_price_store(Exchange::BitflyerCfd, "FX_BTC_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps, true);

    let trade = trader.execute(&make_signal()).await.expect("open");
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close");
    assert_eq!(
        closed.fees,
        Decimal::ZERO,
        "paper commission::estimate=0 + sfd::estimate=0"
    );
    let from_db = auto_trader_db::trades::get_trade_by_id(&pool, closed.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(from_db.fees, Decimal::ZERO);
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_close_continues_when_sfd_fetch_fails(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "sfd_live_err",
        "live",
        "bitflyer_cfd",
        "sfd_test",
        1_000_000,
    )
    .await;
    let api: Arc<dyn ExchangeApi> = Arc::new(SfdMockApi {
        fee_commission: dec!(7),
        sfd: Err("simulated 503 from getpositions"),
    });
    let ps = make_price_store(Exchange::BitflyerCfd, "FX_BTC_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps, false);

    let trade = trader.execute(&make_signal()).await.expect("open");
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should NOT block on sfd fetch error");
    assert_eq!(
        closed.fees,
        dec!(14),
        "open=7 + close commission=7 + sfd=0 (failure swallowed)"
    );
    let from_db = auto_trader_db::trades::get_trade_by_id(&pool, closed.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(from_db.fees, dec!(14));
}
