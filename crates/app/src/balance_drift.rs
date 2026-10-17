use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use auto_trader_core::types::{Exchange, Trade, TradeStatus};
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::PriceStore;
use auto_trader_notify::SystemAlertEvent;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::PgPool;

use crate::positions::build_close_side_positions;

const DRIFT_THRESHOLD_RATIO_OF_EXCHANGE_EQUITY: Decimal = dec!(0.01);
const DRIFT_THRESHOLD_FLOOR_JPY: Decimal = dec!(500);

/// Returns whether `exchange_equity` and `bot_equity` differ by more than the larger of 1% of `exchange_equity` and 500 JPY.
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

/// Dependencies of the balance drift check.
pub struct BalanceDriftContext {
    /// Connection pool of the database.
    pub pool: PgPool,
    /// Store of the latest ticks of the feeds.
    pub price_store: Arc<PriceStore>,
    /// Exchange API of each exchange.
    pub apis: Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>,
    /// Whether live accounts are forced to dry run, in which case the check runs on none.
    pub live_forces_dry_run: bool,
}

/// Returns an alert for each account that is active or has open trades and is not dry run, and whose exchange equity drifts from the bot equity, or none if live accounts are forced to dry run. The exchange equity is the collateral plus the open position PnL; the bot equity is the account balance plus the required margin and the unrealized PnL of its open trades. Returns none, with a warning logged, if the accounts cannot be listed. An account is skipped, with a warning logged, when its exchange is unknown, it has no exchange API, or its collateral, trades or prices cannot be obtained; it is skipped, with an info log, while one of its trades is closing. A warning is logged for each drift.
///
/// # Panics
///
/// Panics if the leverage of an open trade of an account whose bot equity is computed is zero.
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
        if let Some(alert) = check_account(ctx, &account).await {
            alerts.push(alert);
        }
    }

    alerts
}

async fn check_account(
    ctx: &BalanceDriftContext,
    account: &TradingAccount,
) -> Option<SystemAlertEvent> {
    let exchange = match Exchange::from_str(&account.exchange) {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!(
                account_name = %account.name,
                exchange = %account.exchange,
                error = %e,
                "failed to parse exchange, skipping"
            );
            return None;
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
            return None;
        }
    };

    let exchange_equity = fetch_exchange_equity(api.as_ref(), account, exchange).await?;
    let open_trades = load_open_trades_for_equity(ctx, account).await?;
    let bot_equity = compute_bot_equity(ctx, account, &open_trades).await?;

    build_drift_alert(&account.name, exchange, exchange_equity, bot_equity)
}

async fn fetch_exchange_equity(
    api: &dyn ExchangeApi,
    account: &TradingAccount,
    exchange: Exchange,
) -> Option<Decimal> {
    let collateral = match api.get_collateral().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                account_name = %account.name,
                exchange = %exchange,
                error = %e,
                "failed to get collateral, skipping"
            );
            return None;
        }
    };
    Some(collateral.collateral + collateral.open_position_pnl)
}

async fn load_open_trades_for_equity(
    ctx: &BalanceDriftContext,
    account: &TradingAccount,
) -> Option<Vec<Trade>> {
    let trades = match auto_trader_db::trades::list_open_or_closing_by_account(
        &ctx.pool, account.id,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                account_name = %account.name,
                error = %e,
                "failed to read open or closing trades, skipping"
            );
            return None;
        }
    };

    match split_for_equity(trades) {
        EquityInput::CloseInProgress { closing } => {
            tracing::info!(
                account_name = %account.name,
                closing_trades = closing,
                "deferring equity comparison until close completes"
            );
            None
        }
        EquityInput::Open(open) => Some(open),
    }
}

async fn compute_bot_equity(
    ctx: &BalanceDriftContext,
    account: &TradingAccount,
    open_trades: &[Trade],
) -> Option<Decimal> {
    let ctx_label = format!("balance drift: account {}", account.name);
    let mut price_cache = HashMap::new();
    let positions = build_close_side_positions(
        open_trades.iter(),
        &ctx.price_store,
        &mut price_cache,
        &ctx_label,
    )
    .await?;

    let required: Decimal = positions.iter().map(|p| p.required_margin()).sum();
    let unrealized: Decimal = positions.iter().map(|p| p.unrealized_pnl()).sum();
    Some(account.current_balance + required + unrealized)
}

fn build_drift_alert(
    account_name: &str,
    exchange: Exchange,
    exchange_equity: Decimal,
    bot_equity: Decimal,
) -> Option<SystemAlertEvent> {
    if !is_drift(exchange_equity, bot_equity) {
        return None;
    }
    let diff = exchange_equity - bot_equity;
    tracing::warn!(
        account_name = %account_name,
        exchange = %exchange,
        exchange_equity = %exchange_equity,
        bot_equity = %bot_equity,
        diff = %diff,
        "balance drift detected"
    );
    Some(SystemAlertEvent {
        title: "balance drift".to_string(),
        account_name: account_name.to_string(),
        exchange,
        body: format!(
            "exchange equity={exchange_equity} bot equity={bot_equity} diff={diff} \
             (alert only — no auto-correction; operator must reconcile deposits/swaps)"
        ),
    })
}

#[cfg(test)]
mod tests;
