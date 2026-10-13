use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::margin::compute_maintenance_ratio;
use auto_trader_core::types::Exchange;
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_market::price_store::{FeedKey, PriceStore};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::positions::build_close_side_positions;

pub struct LiquidationContext {
    pub pool: PgPool,
    pub price_store: Arc<PriceStore>,
    pub exchange_liquidation_levels: Arc<HashMap<Exchange, Decimal>>,
    pub live_forces_dry_run: bool,
}

fn is_below_liquidation_threshold(ratio: Decimal, threshold: Decimal) -> bool {
    ratio < threshold
}

pub async fn detect_liquidation_targets(
    ctx: &LiquidationContext,
    open_trades: &[OpenTradeWithAccount],
    event: &PriceEvent,
) -> Vec<(Uuid, Vec<Uuid>)> {
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
        if !dry_run {
            continue;
        }

        let account =
            match auto_trader_db::trading_accounts::get_account(&ctx.pool, account_id).await {
                Ok(Some(a)) => a,
                Ok(None) => {
                    tracing::warn!(
                        account_id = %account_id,
                        "invalid account: not found"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(account_id = %account_id, error = %e, "failed to read account");
                    continue;
                }
            };

        let ctx_label = format!("liquidation: account {account_id}");
        let positions_with_all_prices_known = match build_close_side_positions(
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

        let ratio = match compute_maintenance_ratio(
            account.current_balance,
            &positions_with_all_prices_known,
        ) {
            Some(r) => r,
            None => continue,
        };

        if is_below_liquidation_threshold(ratio, threshold) {
            tracing::warn!(
                account_id = %account_id,
                maintenance_ratio = %ratio,
                threshold = %threshold,
                trade_count = trades_in_account.len(),
                "maintenance ratio below threshold, force-closing account"
            );
            let trade_ids: Vec<Uuid> = trades_in_account.iter().map(|t| t.trade.id).collect();
            results.push((account_id, trade_ids));
        }
    }

    results
}
