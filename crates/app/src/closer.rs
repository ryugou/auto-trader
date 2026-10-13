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

pub struct CloseContext {
    pub pool: PgPool,
    pub apis: Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>,
    pub price_store: Arc<PriceStore>,
    pub notifier: Arc<Notifier>,
    pub position_sizer: Arc<PositionSizer>,
    pub liquidation_levels: Arc<HashMap<Exchange, Decimal>>,
    pub trade_tx: mpsc::Sender<TradeEvent>,
}

pub async fn close_trade(
    ctx: &CloseContext,
    trade: &Trade,
    account_name: String,
    account_type: String,
    dry_run: bool,
    exit_reason: ExitReason,
    current_price: Decimal,
) {
    let api: Arc<dyn ExchangeApi> = match ctx.apis.get(&trade.exchange) {
        Some(a) => a.clone(),
        None => {
            if !dry_run {
                tracing::warn!(
                    exchange = %trade.exchange,
                    trade_id = %trade.id,
                    "unsupported exchange"
                );
                return;
            }
            Arc::new(auto_trader_market::null_exchange_api::NullExchangeApi)
        }
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
        Err(e) => log_close_failure_at_appropriate_level(exit_reason, trade.id, &e),
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
