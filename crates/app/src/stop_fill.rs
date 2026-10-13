use auto_trader_core::types::ExitReason;
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_market::exchange_api::StopOrderStatus;
use auto_trader_market::price_store::FeedKey;

use crate::closer::{CloseContext, close_trade};
use crate::startup::effective_dry_run;

pub fn is_stop_detection_target(t: &OpenTradeWithAccount, live_forces_dry_run: bool) -> bool {
    let account_type = t.account_type.as_deref().unwrap_or("paper");
    let dry_run = effective_dry_run(account_type, live_forces_dry_run);
    !dry_run && t.trade.stop_order_id.is_some()
}

pub async fn detect_and_close_stop_fills(
    ctx: &CloseContext,
    open_trades: &[OpenTradeWithAccount],
    live_forces_dry_run: bool,
) {
    for owned in open_trades
        .iter()
        .filter(|t| is_stop_detection_target(t, live_forces_dry_run))
    {
        let trade = &owned.trade;
        let Some(stop_id) = &trade.stop_order_id else {
            continue;
        };
        let api = match ctx.apis.get(&trade.exchange) {
            Some(a) => a.clone(),
            None => {
                tracing::warn!(
                    exchange = %trade.exchange,
                    trade_id = %trade.id,
                    "no exchange api configured, skipping"
                );
                continue;
            }
        };

        match api.stop_order_status(&trade.pair.0, stop_id).await {
            Ok(StopOrderStatus::Executed { price, .. }) => {
                tracing::info!(
                    stop_id = %stop_id,
                    trade_id = %trade.id,
                    price = %price,
                    "stop order executed, closing as sl_hit"
                );
                let account_name = owned
                    .account_name
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let account_type = owned
                    .account_type
                    .clone()
                    .unwrap_or_else(|| "live".to_string());
                let current_price_overridden_by_fill_close = ctx
                    .price_store
                    .latest_bid_ask(&FeedKey::new(trade.exchange, trade.pair.clone()))
                    .await
                    .map(|(bid, _ask)| bid)
                    .unwrap_or(trade.stop_loss);
                let dry_run_excluded_by_stop_detection_target = false;
                close_trade(
                    ctx,
                    trade,
                    account_name,
                    account_type,
                    dry_run_excluded_by_stop_detection_target,
                    ExitReason::SlHit,
                    current_price_overridden_by_fill_close,
                )
                .await;
            }
            Ok(StopOrderStatus::Active) => {}
            Ok(StopOrderStatus::Gone) => {
                tracing::warn!(
                    stop_id = %stop_id,
                    trade_id = %trade.id,
                    "stop order is gone but the trade is still open, position may be unprotected"
                );
            }
            Err(e) => {
                tracing::warn!(
                    trade_id = %trade.id,
                    error = %e,
                    "failed to get stop order status"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
