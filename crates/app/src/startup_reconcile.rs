//! Startup-time reconciliation for live trading accounts.
//!
//! When the process restarts (deploy, crash, OS restart), DB rows for
//! `status IN ('open', 'closing')` on live accounts may not match the
//! exchange's actual state. This module runs once at startup to detect
//! and repair the mismatch. Paper accounts are skipped (no exchange).
//!
//! NOT periodic — only at startup. A mid-session reconciler would be a
//! separate concern.

use auto_trader_core::types::{Direction, ExitReason};
use auto_trader_db::trades;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::{FeedKey, PriceStore};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;

pub async fn reconcile_live_accounts_at_startup(
    pool: &PgPool,
    accounts: &[auto_trader_db::trading_accounts::TradingAccount],
    apis: &HashMap<auto_trader_core::types::Exchange, Arc<dyn ExchangeApi>>,
    price_store: Arc<PriceStore>,
) -> anyhow::Result<()> {
    for account in accounts.iter().filter(|a| a.account_type == "live") {
        let exchange = match resolve_exchange_enum(&account.exchange) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    "startup reconcile: unknown exchange '{}' for account {} — skipping: {e}",
                    account.exchange,
                    account.name
                );
                continue;
            }
        };

        let Some(api) = apis.get(&exchange) else {
            anyhow::bail!(
                "startup reconcile: live account '{}' references exchange '{}' but no ExchangeApi \
                 is registered for it; cannot reconcile live state — refusing to start",
                account.name,
                account.exchange
            );
        };

        reconcile_one_account(pool, account, api.as_ref(), &price_store).await?;
    }
    Ok(())
}

/// Fetch exchange positions with bounded retry + exponential backoff.
///
/// Attempts up to `MAX_ATTEMPTS` times (2s → 4s between tries) to absorb
/// brief API blips without triggering a docker restart-storm. Fatals cleanly
/// on sustained outage after all attempts are exhausted.
async fn get_positions_with_retry(
    api: &dyn ExchangeApi,
    pair: &str,
    account_name: &str,
) -> anyhow::Result<Vec<auto_trader_market::bitflyer_private::ExchangePosition>> {
    const MAX_ATTEMPTS: u32 = 3;
    let mut delay = std::time::Duration::from_secs(2);
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match api.get_positions(pair).await {
            Ok(ps) => return Ok(ps),
            Err(e) => {
                tracing::warn!(
                    "startup reconcile: get_positions({pair}) attempt {}/{} for {} failed: {e}",
                    attempt,
                    MAX_ATTEMPTS,
                    account_name
                );
                last_err = Some(e);
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("get_positions retry exhausted")))
}

async fn reconcile_one_account(
    pool: &PgPool,
    account: &auto_trader_db::trading_accounts::TradingAccount,
    api: &dyn ExchangeApi,
    price_store: &PriceStore,
) -> anyhow::Result<()> {
    let db_trades = trades::list_open_or_closing_by_account(pool, account.id).await?;
    if db_trades.is_empty() {
        tracing::info!(
            "startup reconcile: {} has no open/closing trades",
            account.name
        );
        return Ok(());
    }

    // Gather unique pairs, fetch exchange positions for each — concurrently.
    let mut pairs_set = std::collections::HashSet::new();
    for t in &db_trades {
        pairs_set.insert(t.pair.0.clone());
    }
    let pairs: Vec<String> = pairs_set.into_iter().collect();

    let fetches = pairs.iter().map(|pair| {
        let account_name = account.name.clone();
        async move {
            let ps = get_positions_with_retry(api, pair, &account_name)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "startup reconcile: get_positions({pair}) exhausted retries for {}: {e}",
                        account_name
                    )
                })?;
            Ok::<_, anyhow::Error>((pair.clone(), ps))
        }
    });
    let results = futures_util::future::try_join_all(fetches).await?;
    let exchange_positions: HashMap<
        String,
        Vec<auto_trader_market::bitflyer_private::ExchangePosition>,
    > = results.into_iter().collect();

    for trade in &db_trades {
        let pair_positions = exchange_positions
            .get(&trade.pair.0)
            .cloned()
            .unwrap_or_default();
        let exchange_has_matching = pair_positions.iter().any(|p| {
            match matches_direction(&p.side, &trade.direction) {
                Some(true) => p.size > Decimal::ZERO,
                Some(false) => false,
                None => {
                    tracing::warn!(
                        "startup reconcile: unknown side '{}' in exchange position for {}; assuming position exists (conservative)",
                        p.side, p.product_code
                    );
                    true // conservative: don't force-close
                }
            }
        });

        match (trade.status.as_str(), exchange_has_matching) {
            ("open", true) => {
                tracing::info!(
                    "startup reconcile: trade {} consistent (DB=open, exchange=open)",
                    trade.id
                );
            }
            ("open", false) => {
                tracing::warn!(
                    "startup reconcile: trade {} DB=open but exchange has no matching position; \
                     closing with best-effort exit price",
                    trade.id
                );
                tracing::warn!(
                    "startup reconcile: trade {} force-closed (reason=orphan)",
                    trade.id
                );
                force_close_db_only(pool, trade, price_store).await?;
            }
            ("closing", true) => {
                tracing::warn!(
                    "startup reconcile: trade {} DB=closing but exchange position still open — \
                     resetting to open for retry by normal close monitor",
                    trade.id
                );
                // Revert closing → open so the normal monitor loop will retry close.
                trades::release_close_lock(pool, trade.id).await?;
            }
            ("closing", false) => {
                tracing::warn!(
                    "startup reconcile: trade {} DB=closing and exchange shows no position — \
                     completing Phase 3 with best-effort exit price",
                    trade.id
                );
                tracing::warn!(
                    "startup reconcile: trade {} force-closed (reason=phase3)",
                    trade.id
                );
                force_close_db_only(pool, trade, price_store).await?;
            }
            (other, _) => {
                // Should never happen — list_open_or_closing_by_account filters to
                // status IN ('open', 'closing'). Bail rather than silently skipping.
                anyhow::bail!(
                    "startup reconcile: unexpected status '{}' for trade {}",
                    other,
                    trade.id
                );
            }
        }
    }
    Ok(())
}

async fn force_close_db_only(
    pool: &PgPool,
    trade: &auto_trader_core::types::Trade,
    price_store: &PriceStore,
) -> anyhow::Result<()> {
    // Best-effort exit price: PriceStore mid, fallback to entry_price.
    let feed_key = FeedKey::new(trade.exchange, trade.pair.clone());
    let exit_price = match price_store.latest_bid_ask(&feed_key).await {
        Some((bid, ask)) => (bid + ask) / Decimal::from(2),
        None => {
            tracing::warn!(
                "startup reconcile: no PriceStore data for {:?} {}; \
                 using entry_price as exit_price for trade {} (approximate)",
                trade.exchange,
                trade.pair,
                trade.id
            );
            trade.entry_price
        }
    };

    let pnl = match trade.direction {
        Direction::Long => (exit_price - trade.entry_price) * trade.quantity,
        Direction::Short => (trade.entry_price - exit_price) * trade.quantity,
    };
    // Truncate pnl to whole yen.
    let pnl = pnl.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);

    trades::close_trade_reconciled(pool, trade.id, exit_price, pnl, ExitReason::Reconciled).await?;
    Ok(())
}

fn resolve_exchange_enum(s: &str) -> anyhow::Result<auto_trader_core::types::Exchange> {
    match s {
        "bitflyer_cfd" => Ok(auto_trader_core::types::Exchange::BitflyerCfd),
        "oanda" => Ok(auto_trader_core::types::Exchange::Oanda),
        other => anyhow::bail!("unknown exchange: {}", other),
    }
}

fn matches_direction(side: &str, direction: &Direction) -> Option<bool> {
    match side.trim().to_ascii_uppercase().as_str() {
        "BUY" => Some(*direction == Direction::Long),
        "SELL" => Some(*direction == Direction::Short),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::Direction;

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
    fn resolve_exchange_enum_known() {
        assert!(resolve_exchange_enum("bitflyer_cfd").is_ok());
        assert!(resolve_exchange_enum("oanda").is_ok());
    }

    #[test]
    fn resolve_exchange_enum_unknown() {
        assert!(resolve_exchange_enum("unknown_exchange").is_err());
    }
}

// ---------------------------------------------------------------------------
// S2: startup_reconcile integration tests (sqlx::test + MockExchangeApi)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod reconcile_tests {
    use super::*;
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
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use uuid::Uuid;

    // -----------------------------------------------------------------------
    // MockExchangeApi
    // -----------------------------------------------------------------------

    /// Minimal mock for `ExchangeApi`. Only `get_positions` is implemented;
    /// all other methods panic (reconciler never calls them).
    struct MockExchangeApi {
        /// Map from pair → positions to return once get_positions_failures is
        /// exhausted.
        positions: HashMap<String, Vec<ExchangePosition>>,
        /// How many times `get_positions` should fail before succeeding.
        get_positions_failures_remaining: AtomicU32,
    }

    impl MockExchangeApi {
        fn new(positions: HashMap<String, Vec<ExchangePosition>>, failures: u32) -> Arc<Self> {
            Arc::new(Self {
                positions,
                get_positions_failures_remaining: AtomicU32::new(failures),
            })
        }
    }

    #[async_trait]
    impl ExchangeApi for MockExchangeApi {
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
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

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
        let id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO trading_accounts
                   (id, name, account_type, exchange, strategy,
                    initial_balance, current_balance, leverage, currency)
               VALUES ($1, 'reconcile_test', 'live', 'bitflyer_cfd', 'bb_mean_revert_v1',
                       30000, 30000, 2, 'JPY')"#,
        )
        .bind(id)
        .execute(pool)
        .await
        .expect("seed_live_account failed");
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

    async fn seed_closing_trade(pool: &PgPool, account_id: Uuid) -> Trade {
        let trade = seed_open_trade(pool, account_id).await;
        // Manually transition to 'closing' (mimics acquire_close_lock)
        sqlx::query("UPDATE trades SET status = 'closing' WHERE id = $1")
            .bind(trade.id)
            .execute(pool)
            .await
            .expect("seed_closing_trade: status update failed");
        trade
    }

    fn build_apis(api: Arc<dyn ExchangeApi>) -> HashMap<Exchange, Arc<dyn ExchangeApi>> {
        let mut m = HashMap::new();
        m.insert(Exchange::BitflyerCfd, api);
        m
    }

    fn empty_price_store() -> Arc<PriceStore> {
        PriceStore::new(vec![]) // PriceStore::new already returns Arc<PriceStore>
    }

    // -----------------------------------------------------------------------
    // S2-1: DB=open, exchange has position → noop (consistent)
    // -----------------------------------------------------------------------

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

        reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store)
            .await
            .expect("reconcile must succeed");

        // Trade must remain open
        let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
            .bind(trade.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "open", "consistent trade must remain open");
    }

    // -----------------------------------------------------------------------
    // S2-2: DB=open, exchange has no position → force close DB row
    // -----------------------------------------------------------------------

    #[sqlx::test(migrations = "../../migrations")]
    async fn db_open_exchange_empty_force_closes_db(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let trade = seed_open_trade(&pool, account_id).await;

        // Exchange returns empty positions (position already gone)
        let api = MockExchangeApi::new(HashMap::new(), 0);
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis = build_apis(api);
        let price_store = empty_price_store();

        reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store)
            .await
            .expect("reconcile must succeed");

        // Trade must now be closed
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

    // -----------------------------------------------------------------------
    // S2-3: DB=closing, exchange has position → reset to open
    // -----------------------------------------------------------------------

    #[sqlx::test(migrations = "../../migrations")]
    async fn db_closing_exchange_has_position_resets_to_open(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let trade = seed_closing_trade(&pool, account_id).await;

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

        reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store)
            .await
            .expect("reconcile must succeed");

        // Trade must be reverted to open
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

    // -----------------------------------------------------------------------
    // S2-4: DB=closing, exchange has no position → complete Phase 3 (close DB)
    // -----------------------------------------------------------------------

    #[sqlx::test(migrations = "../../migrations")]
    async fn db_closing_exchange_empty_completes_phase3(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        let trade = seed_closing_trade(&pool, account_id).await;

        // Exchange shows no position → Phase 2 completed before crash
        let api = MockExchangeApi::new(HashMap::new(), 0);
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();
        let accounts = vec![account];
        let apis = build_apis(api);
        let price_store = empty_price_store();

        reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store)
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

    // -----------------------------------------------------------------------
    // S2-5: get_positions retry exhausted → bail
    // -----------------------------------------------------------------------

    #[sqlx::test(migrations = "../../migrations")]
    async fn get_positions_retry_exhausted_bails(pool: PgPool) {
        let account_id = seed_live_account(&pool).await;
        // Seed a trade so the reconciler actually calls get_positions
        let _trade = seed_open_trade(&pool, account_id).await;

        // Fail all 3 attempts (MAX_ATTEMPTS = 3)
        let api = MockExchangeApi::new(HashMap::new(), 3);
        let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
            .await
            .unwrap()
            .unwrap();

        // Virtualize time after DB seeding: the retry logic uses tokio::time::sleep
        // (2s → 4s backoff), which auto-advances when no tasks are runnable under
        // paused time. Pause must come after DB operations since the pool uses real
        // timeouts internally.
        tokio::time::pause();
        let accounts = vec![account];
        let apis = build_apis(api);
        let price_store = empty_price_store();

        let result = reconcile_live_accounts_at_startup(&pool, &accounts, &apis, price_store).await;

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
}
