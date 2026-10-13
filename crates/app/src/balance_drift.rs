use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use auto_trader_core::types::{Exchange, Trade, TradeStatus};
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::PriceStore;
use auto_trader_notify::SystemAlertEvent;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::PgPool;

use crate::positions::build_close_side_positions;

const DRIFT_THRESHOLD_RATIO_OF_EXCHANGE_EQUITY: Decimal = dec!(0.01);
const DRIFT_THRESHOLD_FLOOR_JPY: Decimal = dec!(500);

pub fn is_drift(exchange_equity: Decimal, bot_equity: Decimal) -> bool {
    let threshold =
        (exchange_equity * DRIFT_THRESHOLD_RATIO_OF_EXCHANGE_EQUITY).max(DRIFT_THRESHOLD_FLOOR_JPY);
    (exchange_equity - bot_equity).abs() > threshold
}

enum EquityInput {
    Open(Vec<Trade>),
    CloseInProgress { closing: usize },
}

fn split_for_equity(trades: Vec<Trade>) -> EquityInput {
    let closing = trades
        .iter()
        .filter(|t| t.status == TradeStatus::Closing)
        .count();
    if closing > 0 {
        EquityInput::CloseInProgress { closing }
    } else {
        EquityInput::Open(trades)
    }
}

pub struct BalanceDriftContext {
    pub pool: PgPool,
    pub price_store: Arc<PriceStore>,
    pub apis: Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>,
    pub live_forces_dry_run: bool,
}

pub async fn check_live_accounts(ctx: &BalanceDriftContext) -> Vec<SystemAlertEvent> {
    if ctx.live_forces_dry_run {
        return vec![];
    }

    let accounts =
        match auto_trader_db::trading_accounts::list_active_or_with_open_trades(&ctx.pool).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "failed to list accounts");
                return vec![];
            }
        };

    let mut alerts = Vec::new();

    for account in accounts
        .into_iter()
        .filter(|a| !crate::startup::effective_dry_run(&a.account_type, ctx.live_forces_dry_run))
    {
        let exchange = match Exchange::from_str(&account.exchange) {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(
                    account_name = %account.name,
                    exchange = %account.exchange,
                    error = %e,
                    "failed to parse exchange, skipping"
                );
                continue;
            }
        };

        let api = match ctx.apis.get(&exchange) {
            Some(a) => a,
            None => {
                tracing::warn!(
                    exchange = %exchange,
                    account_name = %account.name,
                    "no exchange api configured, skipping"
                );
                continue;
            }
        };

        let collateral = match api.get_collateral().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    account_name = %account.name,
                    exchange = %exchange,
                    error = %e,
                    "failed to get collateral, skipping"
                );
                continue;
            }
        };
        let exchange_equity = collateral.collateral + collateral.open_position_pnl;

        let trades =
            match auto_trader_db::trades::list_open_or_closing_by_account(&ctx.pool, account.id)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        account_name = %account.name,
                        error = %e,
                        "failed to read open or closing trades, skipping"
                    );
                    continue;
                }
            };

        let open_trades = match split_for_equity(trades) {
            EquityInput::CloseInProgress { closing } => {
                tracing::info!(
                    account_name = %account.name,
                    closing_trades = closing,
                    "deferring equity comparison until close completes"
                );
                continue;
            }
            EquityInput::Open(open) => open,
        };

        let ctx_label = format!("balance drift: account {}", account.name);
        let mut price_cache = HashMap::new();
        let positions = match build_close_side_positions(
            open_trades.iter(),
            &ctx.price_store,
            &mut price_cache,
            &ctx_label,
        )
        .await
        {
            Some(p) => p,
            None => continue,
        };

        let required: Decimal = positions.iter().map(|p| p.required_margin()).sum();
        let unrealized: Decimal = positions.iter().map(|p| p.unrealized_pnl()).sum();
        let bot_equity = account.current_balance + required + unrealized;

        if is_drift(exchange_equity, bot_equity) {
            let diff = exchange_equity - bot_equity;
            tracing::warn!(
                account_name = %account.name,
                exchange = %exchange,
                exchange_equity = %exchange_equity,
                bot_equity = %bot_equity,
                diff = %diff,
                "balance drift detected"
            );
            alerts.push(SystemAlertEvent {
                title: "balance drift".to_string(),
                account_name: account.name.clone(),
                exchange,
                body: format!(
                    "exchange equity={exchange_equity} bot equity={bot_equity} diff={diff} \
                     (alert only — no auto-correction; operator must reconcile deposits/swaps)"
                ),
            });
        }
    }

    alerts
}

#[cfg(test)]
mod tests;
