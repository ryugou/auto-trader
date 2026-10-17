use std::collections::HashMap;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::margin::compute_maintenance_ratio;
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_market::price_store::FeedKey;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

use crate::liquidation::{LiquidationContext, group_open_trades_by_account, is_dry_run_bucket};
use crate::positions::build_close_side_positions;

const CRITICAL_MAINTENANCE_RATIO_MULTIPLIER: Decimal = dec!(1.1);
const WARN_MAINTENANCE_RATIO_MULTIPLIER: Decimal = dec!(1.3);

/// Severity of a margin alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertLevel {
    /// The maintenance ratio is below 1.3 times the liquidation margin level but not below 1.1 times.
    Warn,
    /// The maintenance ratio is below 1.1 times the liquidation margin level.
    Critical,
}

impl AlertLevel {
    /// Returns `"warn"` for `Warn` and `"critical"` for `Critical`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

/// Alert about a live account whose maintenance ratio nears the liquidation margin level.
#[derive(Debug, Clone)]
pub struct MarginAlert {
    /// Identifier of the account.
    pub account_id: Uuid,
    /// Name of the account, or its identifier as text if its first trade carries no name.
    pub account_name: String,
    /// Maintenance ratio of the account.
    pub ratio: Decimal,
    /// Liquidation margin level of the exchange.
    pub threshold: Decimal,
    /// Severity of the alert.
    pub level: AlertLevel,
}

/// Returns `Critical` if `ratio` is below 1.1 times `liquidation_level`, `Warn` if it is below 1.3 times but not below 1.1 times, and `None` otherwise.
pub fn classify_alert_level(ratio: Decimal, liquidation_level: Decimal) -> Option<AlertLevel> {
    let critical = liquidation_level * CRITICAL_MAINTENANCE_RATIO_MULTIPLIER;
    let warn = liquidation_level * WARN_MAINTENANCE_RATIO_MULTIPLIER;
    if ratio < critical {
        Some(AlertLevel::Critical)
    } else if ratio < warn {
        Some(AlertLevel::Warn)
    } else {
        None
    }
}

/// Returns an alert for each account that has open trades on the exchange of `event`, is not dry run, and whose maintenance ratio is below 1.3 times the liquidation margin level of that exchange, and logs a warning for each. Returns nothing if the exchange has no liquidation margin level. An account is skipped, with a warning logged, if it cannot be read, is not found, or has a trade with no latest bid and ask; it is also skipped if its maintenance ratio cannot be computed because the total required margin is zero. It closes nothing.
///
/// # Panics
///
/// Panics if the leverage of a trade of an account whose maintenance ratio is computed is zero.
pub async fn detect_margin_alerts(
    ctx: &LiquidationContext,
    open_trades: &[OpenTradeWithAccount],
    event: &PriceEvent,
) -> Vec<MarginAlert> {
    let threshold = match ctx.exchange_liquidation_levels.get(&event.exchange) {
        Some(t) => *t,
        None => return vec![],
    };

    let buckets = group_open_trades_by_account(open_trades, event.exchange);

    let mut price_cache: HashMap<FeedKey, Option<(Decimal, Decimal)>> = HashMap::new();
    let mut results = Vec::new();

    for (account_id, trades_in_account) in buckets {
        if is_dry_run_bucket(&trades_in_account, ctx.live_forces_dry_run) {
            continue;
        }

        let Some(ratio) =
            maintenance_ratio_for_account(ctx, account_id, &trades_in_account, &mut price_cache)
                .await
        else {
            continue;
        };

        if let Some(alert) = build_margin_alert(account_id, &trades_in_account, ratio, threshold) {
            results.push(alert);
        }
    }

    results
}

async fn maintenance_ratio_for_account(
    ctx: &LiquidationContext,
    account_id: Uuid,
    trades_in_account: &[&OpenTradeWithAccount],
    price_cache: &mut HashMap<FeedKey, Option<(Decimal, Decimal)>>,
) -> Option<Decimal> {
    let account = match auto_trader_db::trading_accounts::get_account(&ctx.pool, account_id).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            tracing::warn!(
                account_id = %account_id,
                "account not found (delete race?), skipping"
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                account_id = %account_id,
                error = %e,
                "failed to read account"
            );
            return None;
        }
    };

    let ctx_label = format!("margin_alert: account {account_id}");
    let positions = build_close_side_positions(
        trades_in_account.iter().map(|owned| &owned.trade),
        &ctx.price_store,
        price_cache,
        &ctx_label,
    )
    .await?;

    compute_maintenance_ratio(account.current_balance, &positions)
}

fn build_margin_alert(
    account_id: Uuid,
    trades_in_account: &[&OpenTradeWithAccount],
    ratio: Decimal,
    threshold: Decimal,
) -> Option<MarginAlert> {
    let level = classify_alert_level(ratio, threshold)?;
    let account_name = trades_in_account
        .first()
        .and_then(|t| t.account_name.clone())
        .unwrap_or_else(|| account_id.to_string());
    tracing::warn!(
        account_id = %account_id,
        maintenance_ratio = %ratio,
        threshold = %threshold,
        level = level.as_str(),
        "live account maintenance ratio alert, no auto-close"
    );
    Some(MarginAlert {
        account_id,
        account_name,
        ratio,
        threshold,
        level,
    })
}

#[cfg(test)]
mod tests;
