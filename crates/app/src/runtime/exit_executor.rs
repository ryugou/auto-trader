use std::sync::Arc;

use auto_trader::closer::CloseContext;
use auto_trader::startup::{effective_dry_run, liquidation_level_or_log};
use auto_trader_core::event::{TradeAction, TradeEvent};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::strategy::ExitSignal;
use auto_trader_core::types::Trade;
use auto_trader_executor::trader::Trader as UnifiedTrader;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::null_exchange_api::NullExchangeApi;
use tokio::sync::mpsc;

use super::{TradeAccount, resolve_trade_account};

pub(crate) async fn run(
    close_ctx: CloseContext,
    live_forces_dry_run: bool,
    mut exit_rx: mpsc::Receiver<ExitSignal>,
) {
    while let Some(exit) = exit_rx.recv().await {
        process_exit(&close_ctx, live_forces_dry_run, &exit).await;
    }
}

async fn process_exit(close_ctx: &CloseContext, live_forces_dry_run: bool, exit: &ExitSignal) {
    let Some((trade, account)) = load_exit_target(close_ctx, exit).await else {
        return;
    };
    let TradeAccount {
        account_type,
        account_name,
    } = account;
    let dry_run = effective_dry_run(&account_type, live_forces_dry_run);
    let Some(api) = resolve_exit_api(close_ctx, &trade, dry_run) else {
        return;
    };
    let Some(trader) = build_exit_trader(close_ctx, &trade, account_name, api, dry_run) else {
        return;
    };
    handle_close_position_result(close_ctx, &trade, &account_type, trader, exit).await;
}

async fn load_exit_target(
    close_ctx: &CloseContext,
    exit: &ExitSignal,
) -> Option<(Trade, TradeAccount)> {
    let owned =
        match auto_trader_db::trades::get_open_trade_with_account(&close_ctx.pool, exit.trade_id)
            .await
        {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::debug!(
                    trade_id = %exit.trade_id,
                    "invalid trade: missing or closed"
                );
                return None;
            }
            Err(e) => {
                tracing::warn!(trade_id = %exit.trade_id, error = %e, "failed to load trade");
                return None;
            }
        };
    let account = resolve_trade_account(&owned)?;
    Some((owned.trade, account))
}

fn resolve_exit_api(
    close_ctx: &CloseContext,
    trade: &Trade,
    dry_run: bool,
) -> Option<Arc<dyn ExchangeApi>> {
    match close_ctx.apis.get(&trade.exchange) {
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
            Some(Arc::new(NullExchangeApi))
        }
    }
}

fn build_exit_trader(
    close_ctx: &CloseContext,
    trade: &Trade,
    account_name: String,
    api: Arc<dyn ExchangeApi>,
    dry_run: bool,
) -> Option<UnifiedTrader> {
    let liquidation_margin_level =
        liquidation_level_or_log(&close_ctx.liquidation_levels, trade.exchange, || {
            format!("strategy exit on trade {}", trade.id)
        })?;
    Some(UnifiedTrader::new(
        close_ctx.pool.clone(),
        trade.exchange,
        trade.account_id,
        account_name,
        api,
        close_ctx.price_store.clone(),
        close_ctx.notifier.clone(),
        close_ctx.position_sizer.clone(),
        liquidation_margin_level,
        dry_run,
    ))
}

async fn handle_close_position_result(
    close_ctx: &CloseContext,
    trade: &Trade,
    account_type: &str,
    trader: UnifiedTrader,
    exit: &ExitSignal,
) {
    let reason = exit.reason.to_exit_reason();
    match trader.close_position(&trade.id.to_string(), reason).await {
        Ok(closed) => {
            tracing::info!(
                strategy_name = %closed.strategy_name,
                pair = %closed.pair,
                direction = ?closed.direction,
                strategy_exit_reason = exit.reason.as_tag(),
                entry_price = %closed.entry_price,
                exit_price = %closed.exit_price.unwrap_or_default(),
                "strategy exit closed position"
            );
            let exit_price = closed.exit_price.unwrap_or(exit.close_price);
            if close_ctx
                .trade_tx
                .send(TradeEvent {
                    trade: closed,
                    action: TradeAction::Closed {
                        exit_price,
                        exit_reason: reason,
                    },
                    account_type: Some(account_type.to_owned()),
                })
                .await
                .is_err()
            {
                tracing::warn!("failed to send trade event: channel closed");
            }
        }
        Err(e) => {
            tracing::warn!(
                trade_id = %trade.id,
                error = %e,
                "failed to close trade for strategy exit"
            );
        }
    }
}
