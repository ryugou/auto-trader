use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::event::{TradeAction, TradeEvent};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Exchange, ExitReason, Trade};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader as UnifiedTrader;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::PriceStore;
use auto_trader_notify::Notifier;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::mpsc;

use crate::startup;

/// Dependencies of closing a trade.
pub struct CloseContext {
    /// Connection pool of the database.
    pub pool: PgPool,
    /// Exchange API of each exchange.
    pub apis: Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>,
    /// Store of the latest ticks of the feeds.
    pub price_store: Arc<PriceStore>,
    /// Notifier that the closing trader sends notifications through.
    pub notifier: Arc<Notifier>,
    /// Position sizer that the closing trader uses.
    pub position_sizer: Arc<PositionSizer>,
    /// Liquidation margin level of each exchange.
    pub liquidation_levels: Arc<HashMap<Exchange, Decimal>>,
    /// Channel the closed trade event is sent on.
    pub trade_tx: mpsc::Sender<TradeEvent>,
}

/// Closes `trade` with a trader of its exchange and account, then sends a closed trade event on `ctx.trade_tx` with the exit price of the closed trade, or `current_price` if it has none. Nothing is closed, with a warning logged, if the exchange has no API and `dry_run` is false, and nothing is closed, with an error logged, if the exchange has no liquidation margin level or a non-positive one. A failure to close is logged and not returned: as a warning for the liquidation exit reason and as a debug message for any other, and a failure to send the event is logged as an error.
pub async fn close_trade(
    ctx: &CloseContext,
    trade: &Trade,
    account_name: String,
    account_type: String,
    dry_run: bool,
    exit_reason: ExitReason,
    current_price: Decimal,
) {
    let Some(api) = resolve_close_api(ctx, trade, dry_run) else {
        return;
    };

    let liquidation_margin_level =
        match startup::liquidation_level_or_log(&ctx.liquidation_levels, trade.exchange, || {
            format!("close trade {}", trade.id)
        }) {
            Some(y) => y,
            None => return,
        };

    let trader = UnifiedTrader::new(
        ctx.pool.clone(),
        trade.exchange,
        trade.account_id,
        account_name,
        api,
        ctx.price_store.clone(),
        ctx.notifier.clone(),
        ctx.position_sizer.clone(),
        liquidation_margin_level,
        dry_run,
    );

    match trader
        .close_position(&trade.id.to_string(), exit_reason)
        .await
    {
        Ok(closed_trade) => {
            publish_closed_trade(ctx, closed_trade, current_price, exit_reason, account_type).await
        }
        Err(e) => log_close_failure_at_appropriate_level(exit_reason, trade.id, &e),
    }
}

/// Returns the exchange API of the exchange of `trade`, or a null API that supports no exchange operation if the exchange has none and `dry_run` is true, or `None`, with a warning logged, if the exchange has none and `dry_run` is false.
pub fn resolve_close_api(
    ctx: &CloseContext,
    trade: &Trade,
    dry_run: bool,
) -> Option<Arc<dyn ExchangeApi>> {
    match ctx.apis.get(&trade.exchange) {
        Some(a) => Some(a.clone()),
        None => {
            if !dry_run {
                tracing::warn!(
                    exchange = %trade.exchange,
                    trade_id = %trade.id,
                    "unsupported exchange"
                );
                return None;
            }
            Some(Arc::new(
                auto_trader_market::null_exchange_api::NullExchangeApi,
            ))
        }
    }
}

async fn publish_closed_trade(
    ctx: &CloseContext,
    closed_trade: Trade,
    current_price: Decimal,
    exit_reason: ExitReason,
    account_type: String,
) {
    let exit_price = closed_trade.exit_price.unwrap_or(current_price);
    tracing::info!(
        strategy_name = %closed_trade.strategy_name,
        pair = %closed_trade.pair,
        direction = ?closed_trade.direction,
        exit_price = %exit_price,
        exit_reason = ?exit_reason,
        "position closed"
    );
    if let Err(e) = ctx
        .trade_tx
        .send(TradeEvent {
            trade: closed_trade,
            action: TradeAction::Closed {
                exit_price,
                exit_reason,
            },
            account_type: Some(account_type),
        })
        .await
    {
        tracing::error!(error = %e, "failed to send trade event for position close");
    }
}

fn log_close_failure_at_appropriate_level(
    exit_reason: ExitReason,
    trade_id: uuid::Uuid,
    e: &anyhow::Error,
) {
    if matches!(exit_reason, ExitReason::Liquidation) {
        tracing::warn!(
            trade_id = %trade_id,
            error = %e,
            "failed to close position"
        );
    } else {
        tracing::debug!(
            trade_id = %trade_id,
            error = %e,
            "failed to close position: likely lost the concurrent-close race"
        );
    }
}
