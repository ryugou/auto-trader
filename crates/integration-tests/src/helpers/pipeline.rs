use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{Candle, Exchange, Pair, Signal, Trade};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_market::null_exchange_api::NullExchangeApi;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use uuid::Uuid;

const PAPER_TRADING_DRY_RUN: bool = true;

const LEVERAGE_HARDCODED_BY_SEED_TRADING_ACCOUNT: Decimal = Decimal::TWO;

/// Harness of the full-pipeline tests, which holds a seeded paper account, a price store with one feed and a dry-run trader for that account.
pub struct PipelineHarness {
    /// Database pool that the account and the trader use.
    pub pool: PgPool,
    /// Id of the seeded account.
    pub account_id: Uuid,
    /// Name of the seeded account.
    pub account_name: String,
    /// Exchange of the account and of the price feed.
    pub exchange: Exchange,
    /// Pair of the price feed.
    pub pair: Pair,
    /// Leverage of the seeded account.
    pub leverage: Decimal,
    /// Initial balance of the seeded account.
    pub balance: Decimal,
    /// Liquidation margin level that the trader passes to the position sizer.
    pub liquidation_margin_level: Decimal,
    /// Price store whose only feed is the pair on the exchange.
    pub price_store: Arc<PriceStore>,
    /// Trader of the account, which runs in dry-run mode.
    pub trader: Trader,
}

/// Settings of the account, the price feed and the trader that `PipelineHarness::new` builds.
pub struct PipelineHarnessConfig {
    /// Name of the account to seed.
    pub account_name: String,
    /// Exchange of the account and of the price feed.
    pub exchange: Exchange,
    /// Name of the pair of the price feed.
    pub pair_str: String,
    /// Name of the strategy that the account is bound to.
    pub strategy: String,
    /// Initial balance of the account.
    pub balance: i64,
    /// Liquidation margin level that the trader passes to the position sizer.
    pub liquidation_margin_level: Decimal,
    /// Minimum order size of the pair.
    pub min_order_size: Decimal,
}

impl PipelineHarness {
    /// Seeds a paper account, builds a price store with one feed for the pair on the exchange, and builds a dry-run trader for the account.
    ///
    /// # Errors
    ///
    /// - Returns an error if seeding the account fails.
    pub async fn new(pool: PgPool, cfg: PipelineHarnessConfig) -> anyhow::Result<Self> {
        use crate::helpers::db::seed_trading_account;

        let account_id = seed_trading_account(
            &pool,
            &cfg.account_name,
            "paper",
            cfg.exchange.as_str(),
            &cfg.strategy,
            cfg.balance,
        )
        .await?;

        let pair = Pair::new(&cfg.pair_str);
        let feed_key = FeedKey::new(cfg.exchange, pair.clone());
        let price_store = PriceStore::new(vec![feed_key]);

        let mut min_sizes: HashMap<Pair, Decimal> = HashMap::new();
        min_sizes.insert(pair.clone(), cfg.min_order_size);
        let sizer = Arc::new(PositionSizer::new(min_sizes, rust_decimal::Decimal::ZERO));

        let api: Arc<dyn auto_trader_market::exchange_api::ExchangeApi> = Arc::new(NullExchangeApi);
        let notifier = Arc::new(Notifier::new_disabled());

        let trader = Trader::new(
            pool.clone(),
            cfg.exchange,
            account_id,
            cfg.account_name.clone(),
            api,
            price_store.clone(),
            notifier,
            sizer,
            cfg.liquidation_margin_level,
            PAPER_TRADING_DRY_RUN,
        );

        Ok(Self {
            pool,
            account_id,
            account_name: cfg.account_name,
            exchange: cfg.exchange,
            pair,
            leverage: LEVERAGE_HARDCODED_BY_SEED_TRADING_ACCOUNT,
            balance: Decimal::from(cfg.balance),
            liquidation_margin_level: cfg.liquidation_margin_level,
            price_store,
            trader,
        })
    }

    /// Sets the latest tick of the feed to `bid` and `ask`, with their mean as its price and the current time as its timestamp.
    pub async fn set_market(&self, bid: Decimal, ask: Decimal) {
        let feed_key = FeedKey::new(self.exchange, self.pair.clone());
        self.price_store
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
    }

    /// Warms up the strategy with the `warmup` candles and returns the signal that it emits for the `trigger` candle, if any.
    pub async fn drive_strategy(
        &self,
        strategy: &mut dyn Strategy,
        warmup: &[Candle],
        trigger: &Candle,
    ) -> Option<Signal> {
        let warmup_events: Vec<PriceEvent> = warmup
            .iter()
            .map(|c| make_event(c.clone(), self.exchange))
            .collect();
        strategy.warmup(&warmup_events).await;

        let trigger_event = make_event(trigger.clone(), self.exchange);
        strategy.on_price(&trigger_event).await
    }

    /// Executes the signal through the trader and returns the opened trade.
    ///
    /// # Errors
    ///
    /// - Returns an error if the trader fails to execute the signal.
    pub async fn execute(&self, signal: &Signal) -> anyhow::Result<Trade> {
        self.trader
            .execute(signal)
            .await
            .context("failed to execute signal")
    }

    /// Closes the trade through the trader with the given exit reason and returns the closed trade.
    ///
    /// # Errors
    ///
    /// - Returns an error if the trader fails to close the trade.
    pub async fn close(
        &self,
        trade_id: Uuid,
        reason: auto_trader_core::types::ExitReason,
    ) -> anyhow::Result<Trade> {
        self.trader
            .close_position(&trade_id.to_string(), reason)
            .await
            .context("failed to close position")
    }

    /// Returns the current balance of the account.
    ///
    /// # Errors
    ///
    /// - Returns an error if the query fails.
    pub async fn current_balance(&self) -> anyhow::Result<Decimal> {
        let row: (Decimal,) =
            sqlx::query_as("SELECT current_balance FROM trading_accounts WHERE id = $1")
                .bind(self.account_id)
                .fetch_one(&self.pool)
                .await
                .context("failed to read current balance")?;
        Ok(row.0)
    }

    /// Returns the events of the account, ordered by the time they occurred.
    ///
    /// # Errors
    ///
    /// - Returns an error if the query fails.
    pub async fn events(&self) -> anyhow::Result<Vec<AccountEvent>> {
        sqlx::query_as::<_, AccountEvent>(
            r#"SELECT event_type, amount, balance_after
               FROM account_events
               WHERE account_id = $1
               ORDER BY occurred_at"#,
        )
        .bind(self.account_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to read account events")
    }
}

/// Row of `account_events`, which records a change of the balance of an account.
#[derive(sqlx::FromRow, Debug, PartialEq)]
pub struct AccountEvent {
    /// Type of the event.
    pub event_type: String,
    /// Amount by which the event changed the balance.
    pub amount: Decimal,
    /// Balance of the account after the event.
    pub balance_after: Decimal,
}

fn make_event(candle: Candle, exchange: Exchange) -> PriceEvent {
    PriceEvent {
        pair: candle.pair.clone(),
        exchange,
        timestamp: candle.timestamp,
        candle,
        indicators: HashMap::new(),
    }
}

#[cfg(test)]
mod tests;
