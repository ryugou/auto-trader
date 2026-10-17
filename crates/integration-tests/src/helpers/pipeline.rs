use std::collections::HashMap;
use std::sync::Arc;

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

/// Test harness for full-pipeline integration tests. Wires up the DB pool,
/// price store, sizer, trader, and a strategy instance for one account.
pub struct PipelineHarness {
    pub pool: PgPool,
    pub account_id: Uuid,
    pub account_name: String,
    pub exchange: Exchange,
    pub pair: Pair,
    pub leverage: Decimal,
    pub balance: Decimal,
    pub liquidation_margin_level: Decimal,
    pub price_store: Arc<PriceStore>,
    pub trader: Trader,
}

pub struct PipelineHarnessConfig {
    pub account_name: String,
    pub exchange: Exchange,
    pub pair_str: String,
    pub strategy: String,
    pub balance: i64,
    pub liquidation_margin_level: Decimal,
    pub min_order_size: Decimal,
}

impl PipelineHarness {
    /// Seed an account, build a price store with the given pair and a
    /// trader configured for `dry_run` paper trading.
    pub async fn new(pool: PgPool, cfg: PipelineHarnessConfig) -> Self {
        use crate::helpers::db::seed_trading_account;

        let account_id = seed_trading_account(
            &pool,
            &cfg.account_name,
            "paper",
            cfg.exchange.as_str(),
            &cfg.strategy,
            cfg.balance,
        )
        .await;

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

        Self {
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
        }
    }

    /// Push a tick (bid/ask) into the price store so `trader.execute` can
    /// see a fill price.
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

    /// Drive a strategy through warmup candles + 1 trigger candle. Returns
    /// the signal emitted (if any).
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

    /// Execute a signal through the full trader path. Asserts success.
    pub async fn execute(&self, signal: &Signal) -> Trade {
        self.trader
            .execute(signal)
            .await
            .expect("failed to execute signal")
    }

    /// Close a trade through `trader.close_position`. Asserts success.
    pub async fn close(
        &self,
        trade_id: Uuid,
        reason: auto_trader_core::types::ExitReason,
    ) -> Trade {
        self.trader
            .close_position(&trade_id.to_string(), reason)
            .await
            .expect("failed to close position")
    }

    /// Read `account.current_balance` from DB.
    pub async fn current_balance(&self) -> Decimal {
        let row: (Decimal,) =
            sqlx::query_as("SELECT current_balance FROM trading_accounts WHERE id = $1")
                .bind(self.account_id)
                .fetch_one(&self.pool)
                .await
                .expect("failed to read current balance");
        row.0
    }

    /// Read `account_events` for this account, ordered by `occurred_at`.
    pub async fn events(&self) -> Vec<AccountEvent> {
        sqlx::query_as::<_, AccountEvent>(
            r#"SELECT event_type, amount, balance_after
               FROM account_events
               WHERE account_id = $1
               ORDER BY occurred_at"#,
        )
        .bind(self.account_id)
        .fetch_all(&self.pool)
        .await
        .expect("failed to read account events")
    }
}

#[derive(sqlx::FromRow, Debug, PartialEq)]
pub struct AccountEvent {
    pub event_type: String,
    pub amount: Decimal,
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
