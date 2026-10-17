use std::sync::Arc;

use auto_trader::startup::{FxNewAccountMismatch, FxNewMismatchKind};
use auto_trader_core::types::Exchange;
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_notify::{Notifier, NotifyEvent, SystemAlertEvent};
use rust_decimal::Decimal;

pub(crate) fn alert_mismatches(db_accounts: &[TradingAccount], notifier: &Arc<Notifier>) {
    for mismatch in auto_trader::startup::check_fx_new_account_assumptions(db_accounts) {
        alert_one(&mismatch, notifier);
    }
}

fn alert_one(mismatch: &FxNewAccountMismatch, notifier: &Arc<Notifier>) {
    let notify_exchange = resolve_notify_exchange(&mismatch.account_exchange);
    match &mismatch.kind {
        FxNewMismatchKind::Leverage {
            account_leverage,
            expected_leverage,
        } => {
            let body = format_leverage_mismatch_body(
                &mismatch.account_name,
                account_leverage,
                expected_leverage,
            );
            tracing::error!(body = %body, "invalid leverage: mismatch with fx_new");
            alert(
                notifier,
                "fx_new leverage mismatch",
                &mismatch.account_name,
                notify_exchange,
                body,
            );
        }
        FxNewMismatchKind::Exchange { expected_exchange } => {
            let body = format_exchange_mismatch_body(
                &mismatch.account_name,
                &mismatch.account_exchange,
                *expected_exchange,
            );
            tracing::error!(body = %body, "invalid exchange: mismatch with fx_new");
            alert(
                notifier,
                "fx_new exchange mismatch",
                &mismatch.account_name,
                notify_exchange,
                body,
            );
        }
    }
}

fn resolve_notify_exchange(account_exchange: &str) -> Exchange {
    account_exchange
        .parse::<Exchange>()
        .unwrap_or(auto_trader_strategy::fx_new::FX_NEW_EXCHANGE)
}

fn format_leverage_mismatch_body(
    account_name: &str,
    account_leverage: &Decimal,
    expected_leverage: &Decimal,
) -> String {
    format!(
        "account '{}' has leverage {} but fx_new strategy assumes {} — risk sizing (1% of equity at stop) is wrong for this account until leverage is corrected",
        account_name, account_leverage, expected_leverage
    )
}

fn format_exchange_mismatch_body(
    account_name: &str,
    account_exchange: &str,
    expected_exchange: Exchange,
) -> String {
    format!(
        "account '{}' has exchange '{}' but fx_new only processes {} events — the executor will not dispatch fx_new signals to this account until the exchange is corrected, so no order will be placed for it",
        account_name,
        account_exchange,
        expected_exchange.as_str()
    )
}

fn alert(
    notifier: &Arc<Notifier>,
    title: &str,
    account_name: &str,
    exchange: Exchange,
    body: String,
) {
    auto_trader::spawn_notify(
        notifier,
        NotifyEvent::SystemAlert(SystemAlertEvent {
            title: title.to_string(),
            account_name: account_name.to_string(),
            exchange,
            body,
        }),
    );
}

#[cfg(test)]
mod tests;
