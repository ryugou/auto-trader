use auto_trader_core::types::Direction;

use super::*;

#[test]
fn matches_direction_long() {
    assert_eq!(matches_direction("BUY", &Direction::Long), Some(true));
    assert_eq!(matches_direction("buy", &Direction::Long), Some(true));
    assert_eq!(matches_direction("SELL", &Direction::Long), Some(false));
}

#[test]
fn matches_direction_short() {
    assert_eq!(matches_direction("SELL", &Direction::Short), Some(true));
    assert_eq!(matches_direction("sell", &Direction::Short), Some(true));
    assert_eq!(matches_direction("BUY", &Direction::Short), Some(false));
}

#[test]
fn matches_direction_unknown_side() {
    assert_eq!(matches_direction("UNKNOWN", &Direction::Long), None);
    assert_eq!(matches_direction("", &Direction::Short), None);
}

#[test]
fn exchange_from_str_known() {
    assert!(
        "bitflyer_cfd"
            .parse::<auto_trader_core::types::Exchange>()
            .is_ok()
    );
    assert!("oanda".parse::<auto_trader_core::types::Exchange>().is_ok());
    assert!(
        "gmo_fx"
            .parse::<auto_trader_core::types::Exchange>()
            .is_ok()
    );
}

#[test]
fn exchange_from_str_unknown() {
    assert!(
        "unknown_exchange"
            .parse::<auto_trader_core::types::Exchange>()
            .is_err()
    );
}

mod reconcile_tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use auto_trader_core::types::{Exchange, Pair, Trade, TradeStatus};
    use auto_trader_market::bitflyer_private::{
        ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
        SendChildOrderResponse,
    };
    use auto_trader_market::exchange_api::ExchangeApi;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;

    struct MockExchangeApi {
        positions: HashMap<String, Vec<ExchangePosition>>,
        get_positions_failures_remaining: AtomicU32,
        stop_status: Option<StopOrderStatus>,
    }

    impl MockExchangeApi {
        fn new(positions: HashMap<String, Vec<ExchangePosition>>, failures: u32) -> Arc<Self> {
            Arc::new(Self {
                positions,
                get_positions_failures_remaining: AtomicU32::new(failures),
                stop_status: None,
            })
        }

        fn new_with_stop(
            positions: HashMap<String, Vec<ExchangePosition>>,
            stop_status: StopOrderStatus,
        ) -> Arc<Self> {
            Arc::new(Self {
                positions,
                get_positions_failures_remaining: AtomicU32::new(0),
                stop_status: Some(stop_status),
            })
        }
    }

    fn test_notifier() -> Arc<Notifier> {
        Arc::new(Notifier::new_disabled())
    }

    #[async_trait]
    impl ExchangeApi for MockExchangeApi {
        async fn get_positions(&self, product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
            if self
                .get_positions_failures_remaining
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
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
            unimplemented!("MockExchangeApi: send_child_order not used in reconciler tests")
        }

        async fn get_child_orders(
            &self,
            _product_code: &str,
            _child_order_acceptance_id: &str,
        ) -> anyhow::Result<Vec<ChildOrder>> {
            unimplemented!("MockExchangeApi: get_child_orders not used in reconciler tests")
        }

        async fn get_executions(
            &self,
            _product_code: &str,
            _child_order_acceptance_id: &str,
        ) -> anyhow::Result<Vec<Execution>> {
            unimplemented!("MockExchangeApi: get_executions not used in reconciler tests")
        }

        async fn get_collateral(&self) -> anyhow::Result<Collateral> {
            unimplemented!("MockExchangeApi: get_collateral not used in reconciler tests")
        }

        async fn cancel_child_order(
            &self,
            _product_code: &str,
            _child_order_acceptance_id: &str,
        ) -> anyhow::Result<()> {
            unimplemented!("MockExchangeApi: cancel_child_order not used in reconciler tests")
        }

        async fn resolve_position_id(
            &self,
            _product_code: &str,
            _after: chrono::DateTime<chrono::Utc>,
            _expected_side: auto_trader_market::bitflyer_private::Side,
            _expected_size: rust_decimal::Decimal,
        ) -> anyhow::Result<Option<String>> {
            Ok(None)
        }

        async fn stop_order_status(
            &self,
            _product_code: &str,
            _stop_order_id: &str,
        ) -> anyhow::Result<StopOrderStatus> {
            self.stop_status
                .clone()
                .ok_or_else(|| anyhow::anyhow!("MockExchangeApi: stop_status not configured"))
        }
    }

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

    async fn seed_live_account(pool: &PgPool) -> Uuid {
        seed_account_with_exchange(pool, "live", "bitflyer_cfd").await
    }

    async fn seed_account_with_exchange(pool: &PgPool, account_type: &str, exchange: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO trading_accounts
                   (id, name, account_type, exchange, strategy,
                    initial_balance, current_balance, leverage, currency)
               VALUES ($1, 'reconcile_test', $2, $3, 'bb_mean_revert_v1',
                       30000, 30000, 2, 'JPY')"#,
        )
        .bind(id)
        .bind(account_type)
        .bind(exchange)
        .execute(pool)
        .await
        .expect("seed_account_with_exchange failed");
        id
    }

    async fn seed_open_trade(pool: &PgPool, account_id: Uuid) -> Trade {
        let trade = Trade {
            id: Uuid::new_v4(),
            account_id,
            strategy_name: "bb_mean_revert_v1".to_string(),
            pair: Pair::new("FX_BTC_JPY"),
            exchange: Exchange::BitflyerCfd,
            direction: Direction::Long,
            entry_price: dec!(11_500_000),
            exit_price: None,
            stop_loss: dec!(11_155_000),
            take_profit: None,
            quantity: dec!(0.001),
            leverage: dec!(2),
            fees: dec!(0),
            entry_at: Utc::now(),
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: None,
            exchange_position_id: None,
            stop_order_id: None,
        };
        sqlx::query(
            r#"INSERT INTO trades
                   (id, account_id, strategy_name, pair, exchange, direction,
                    entry_price, exit_price, stop_loss, take_profit,
                    quantity, leverage, fees, entry_at, exit_at,
                    pnl_amount, exit_reason, status, max_hold_until)
               VALUES ($1, $2, $3, $4, $5, $6,
                       $7, $8, $9, $10,
                       $11, $12, $13, $14, $15,
                       $16, $17, $18, $19)"#,
        )
        .bind(trade.id)
        .bind(trade.account_id)
        .bind(&trade.strategy_name)
        .bind(&trade.pair.0)
        .bind(trade.exchange.as_str())
        .bind("long")
        .bind(trade.entry_price)
        .bind(trade.exit_price)
        .bind(trade.stop_loss)
        .bind(trade.take_profit)
        .bind(trade.quantity)
        .bind(trade.leverage)
        .bind(trade.fees)
        .bind(trade.entry_at)
        .bind(trade.exit_at)
        .bind(trade.pnl_amount)
        .bind(Option::<String>::None)
        .bind("open")
        .bind(trade.max_hold_until)
        .execute(pool)
        .await
        .expect("seed_open_trade failed");
        trade
    }

    async fn seed_open_trade_with_stop(
        pool: &PgPool,
        account_id: Uuid,
        stop_order_id: &str,
    ) -> Trade {
        let mut trade = seed_open_trade(pool, account_id).await;
        sqlx::query("UPDATE trades SET stop_order_id = $1 WHERE id = $2")
            .bind(stop_order_id)
            .bind(trade.id)
            .execute(pool)
            .await
            .expect("set stop_order_id failed");
        trade.stop_order_id = Some(stop_order_id.to_string());
        trade
    }

    async fn seed_closing_trade_without_acquire_close_lock(
        pool: &PgPool,
        account_id: Uuid,
    ) -> Trade {
        let trade = seed_open_trade(pool, account_id).await;
        sqlx::query("UPDATE trades SET status = 'closing' WHERE id = $1")
            .bind(trade.id)
            .execute(pool)
            .await
            .expect("seed_closing_trade_without_acquire_close_lock: status update failed");
        trade
    }

    fn build_apis(api: Arc<dyn ExchangeApi>) -> HashMap<Exchange, Arc<dyn ExchangeApi>> {
        let mut m = HashMap::new();
        m.insert(Exchange::BitflyerCfd, api);
        m
    }

    fn empty_price_store() -> Arc<PriceStore> {
        PriceStore::new(vec![])
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn db_open_exchange_has_position_is_noop(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let trade = seed_open_trade(&pool, account_id).await;

        let mut positions = HashMap::new();
        positions.insert(
            "FX_BTC_JPY".to_string(),
            vec![make_exchange_position("BUY", dec!(0.001))],
        );
        let api = MockExchangeApi::new(positions, 0);
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis = build_apis(api);
        let price_store = empty_price_store();

        reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store, test_notifier())
            .await
            .expect("reconcile must succeed");

        let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
            .bind(trade.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "open", "consistent trade must remain open");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn db_open_exchange_empty_force_closes_db(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let trade = seed_open_trade(&pool, account_id).await;

        let api = MockExchangeApi::new(HashMap::new(), 0);
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis = build_apis(api);
        let price_store = empty_price_store();

        reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store, test_notifier())
            .await
            .expect("reconcile must succeed");

        let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
            .bind(trade.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "closed", "orphan-open trade must be force-closed");

        let exit_reason: String =
            sqlx::query_scalar("SELECT exit_reason FROM trades WHERE id = $1")
                .bind(trade.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            exit_reason, "reconciled",
            "exit_reason must be 'reconciled' for startup-reconcile force-close"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn db_open_exchange_empty_stop_executed_closes_as_sl_hit(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let trade = seed_open_trade_with_stop(&pool, account_id, "stop-xyz").await;

        let api = MockExchangeApi::new_with_stop(
            HashMap::new(),
            StopOrderStatus::Executed {
                price: dec!(11_100_000),
                commission: dec!(50),
            },
        );
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis = build_apis(api);
        let price_store = empty_price_store();

        reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store, test_notifier())
            .await
            .expect("reconcile must succeed");

        let row: (String, Option<String>, Option<Decimal>, Decimal) = sqlx::query_as(
            "SELECT status, exit_reason, exit_price, fees FROM trades WHERE id = $1",
        )
        .bind(trade.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "closed");
        assert_eq!(
            row.1.as_deref(),
            Some("sl_hit"),
            "stop-executed reconcile records exit_reason=sl_hit"
        );
        assert_eq!(
            row.2,
            Some(dec!(11_100_000)),
            "exit_price is the exact stop fill price"
        );
        assert_eq!(row.3, dec!(50), "commission is added to fees");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn db_closing_exchange_has_position_resets_to_open(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let trade = seed_closing_trade_without_acquire_close_lock(&pool, account_id).await;

        let mut positions = HashMap::new();
        positions.insert(
            "FX_BTC_JPY".to_string(),
            vec![make_exchange_position("BUY", dec!(0.001))],
        );
        let api = MockExchangeApi::new(positions, 0);
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis = build_apis(api);
        let price_store = empty_price_store();

        reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store, test_notifier())
            .await
            .expect("reconcile must succeed");

        let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
            .bind(trade.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            status, "open",
            "closing trade with exchange position should be reset to open"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn db_closing_exchange_empty_completes_phase3(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let trade = seed_closing_trade_without_acquire_close_lock(&pool, account_id).await;

        let api = MockExchangeApi::new(HashMap::new(), 0);
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis = build_apis(api);
        let price_store = empty_price_store();

        reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store, test_notifier())
            .await
            .expect("reconcile must succeed");

        let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
            .bind(trade.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            status, "closed",
            "closing trade with no exchange position must be force-closed"
        );

        let exit_reason: String =
            sqlx::query_scalar("SELECT exit_reason FROM trades WHERE id = $1")
                .bind(trade.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            exit_reason, "reconciled",
            "exit_reason must be 'reconciled' for startup-reconcile phase3 force-close"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn get_positions_retry_exhausted_bails_with_real_time_backoff(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let _trade_triggers_get_positions_call = seed_open_trade(&pool, account_id).await;

        let api = MockExchangeApi::new(HashMap::new(), 3);
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();

        let accounts = vec![account];
        let apis = build_apis(api);
        let price_store = empty_price_store();

        let result = reconcile_live_accounts_at_startup(
            &pool,
            &accounts,
            &apis,
            price_store,
            test_notifier(),
        )
        .await;

        assert!(
            result.is_err(),
            "reconcile must fail when get_positions exhausts retries"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exhausted")
                || err_msg.contains("retry")
                || err_msg.contains("retries"),
            "error must mention retry exhaustion: {err_msg}"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn live_account_with_unparseable_exchange_is_rejected(pool: PgPool) {
        let account_id = seed_account_with_exchange(&pool, "live", "not_a_real_exchange").await;
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis: HashMap<Exchange, Arc<dyn ExchangeApi>> = HashMap::new();
        let price_store = empty_price_store();

        let result = reconcile_live_accounts_at_startup(
            &pool,
            &accounts,
            &apis,
            price_store,
            test_notifier(),
        )
        .await;

        let err =
            result.expect_err("reconcile must reject a live account with an unparseable exchange");
        let msg = format!("{err}");
        assert!(
            msg.contains("not_a_real_exchange"),
            "error must name the offending exchange value: {msg}"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn live_account_on_exchange_without_registered_api_is_rejected(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis: HashMap<Exchange, Arc<dyn ExchangeApi>> = HashMap::new();
        let price_store = empty_price_store();

        let result = reconcile_live_accounts_at_startup(
            &pool,
            &accounts,
            &apis,
            price_store,
            test_notifier(),
        )
        .await;

        let err = result.expect_err(
            "reconcile must reject when no ExchangeApi is registered for the account's exchange",
        );
        let msg = format!("{err}");
        assert!(
            msg.contains("bitflyer_cfd"),
            "error must mention the exchange missing an ExchangeApi: {msg}"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn paper_account_is_not_reconciled(pool: PgPool) {
        let account_id = seed_account_with_exchange(&pool, "paper", "bitflyer_cfd").await;
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis_with_no_registered_exchange: HashMap<Exchange, Arc<dyn ExchangeApi>> =
            HashMap::new();
        let price_store = empty_price_store();

        reconcile_live_accounts_at_startup(
            &pool,
            &accounts,
            &apis_with_no_registered_exchange,
            price_store,
            test_notifier(),
        )
        .await
        .expect("paper accounts must be skipped, not reconciled even without a registered api");
    }
}
