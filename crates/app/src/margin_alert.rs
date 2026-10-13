use std::collections::HashMap;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::margin::compute_maintenance_ratio;
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_market::price_store::FeedKey;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

use crate::liquidation::LiquidationContext;
use crate::positions::build_close_side_positions;

const CRITICAL_MAINTENANCE_RATIO_MULTIPLIER: Decimal = dec!(1.1);
const WARN_MAINTENANCE_RATIO_MULTIPLIER: Decimal = dec!(1.3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertLevel {
    Warn,
    Critical,
}

impl AlertLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MarginAlert {
    pub account_id: Uuid,
    pub account_name: String,
    pub ratio: Decimal,
    pub threshold: Decimal,
    pub level: AlertLevel,
}

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

pub async fn detect_margin_alerts(
    ctx: &LiquidationContext,
    open_trades: &[OpenTradeWithAccount],
    event: &PriceEvent,
) -> Vec<MarginAlert> {
    let threshold = match ctx.exchange_liquidation_levels.get(&event.exchange) {
        Some(t) => *t,
        None => return vec![],
    };

    let mut buckets: HashMap<Uuid, Vec<&OpenTradeWithAccount>> = HashMap::new();
    for owned in open_trades
        .iter()
        .filter(|t| t.trade.exchange == event.exchange)
    {
        buckets
            .entry(owned.trade.account_id)
            .or_default()
            .push(owned);
    }

    let mut price_cache: HashMap<FeedKey, Option<(Decimal, Decimal)>> = HashMap::new();
    let mut results = Vec::new();

    for (account_id, trades_in_account) in buckets {
        let account_type = trades_in_account
            .first()
            .and_then(|t| t.account_type.as_deref())
            .unwrap_or("paper");
        let dry_run = crate::startup::effective_dry_run(account_type, ctx.live_forces_dry_run);
        if dry_run {
            continue;
        }

        let account =
            match auto_trader_db::trading_accounts::get_account(&ctx.pool, account_id).await {
                Ok(Some(a)) => a,
                Ok(None) => {
                    tracing::warn!(
                        account_id = %account_id,
                        "account not found (delete race?), skipping"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        account_id = %account_id,
                        error = %e,
                        "failed to read account"
                    );
                    continue;
                }
            };

        let ctx_label = format!("margin_alert: account {account_id}");
        let positions = match build_close_side_positions(
            trades_in_account.iter().map(|owned| &owned.trade),
            &ctx.price_store,
            &mut price_cache,
            &ctx_label,
        )
        .await
        {
            Some(p) => p,
            None => continue,
        };

        let ratio = match compute_maintenance_ratio(account.current_balance, &positions) {
            Some(r) => r,
            None => continue,
        };

        if let Some(level) = classify_alert_level(ratio, threshold) {
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
            results.push(MarginAlert {
                account_id,
                account_name,
                ratio,
                threshold,
                level,
            });
        }
    }

    results
}

#[cfg(test)]
mod tests;
