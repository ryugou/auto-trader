//! Startup-time reconciliation for live trading accounts.
//!
//! When the process restarts (deploy, crash, OS restart), DB rows for
//! `status IN ('open', 'closing')` on live accounts may not match the
//! exchange's actual state. This module runs once at startup to detect
//! and repair the mismatch. Paper accounts are skipped (no exchange).
//!
//! NOT periodic — only at startup. A mid-session reconciler would be a
//! separate concern.

use auto_trader_core::types::Direction;
use auto_trader_db::trades;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::{FeedKey, PriceStore};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;

pub async fn reconcile_live_accounts_at_startup(
    pool: &PgPool,
    accounts: &[auto_trader_db::trading_accounts::TradingAccount],
    apis: &HashMap<auto_trader_core::types::Exchange, Arc<dyn ExchangeApi>>,
    price_store: Arc<PriceStore>,
) -> anyhow::Result<()> {
    for account in accounts.iter().filter(|a| a.account_type == "live") {
        let exchange = match resolve_exchange_enum(&account.exchange) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    "startup reconcile: unknown exchange '{}' for account {} — skipping: {e}",
                    account.exchange,
                    account.name
                );
                continue;
            }
        };

        let Some(api) = apis.get(&exchange) else {
            tracing::warn!(
                "startup reconcile: no ExchangeApi for {} (exchange={}); skipping live account",
                account.name,
                account.exchange
            );
            continue;
        };

        reconcile_one_account(pool, account, api.as_ref(), &price_store).await?;
    }
    Ok(())
}

async fn reconcile_one_account(
    pool: &PgPool,
    account: &auto_trader_db::trading_accounts::TradingAccount,
    api: &dyn ExchangeApi,
    price_store: &PriceStore,
) -> anyhow::Result<()> {
    let db_trades = trades::list_open_or_closing_by_account(pool, account.id).await?;
    if db_trades.is_empty() {
        tracing::info!(
            "startup reconcile: {} has no open/closing trades",
            account.name
        );
        return Ok(());
    }

    // Gather unique pairs, fetch exchange positions for each.
    let mut pairs = std::collections::HashSet::new();
    for t in &db_trades {
        pairs.insert(t.pair.0.clone());
    }

    let mut exchange_positions: HashMap<
        String,
        Vec<auto_trader_market::bitflyer_private::ExchangePosition>,
    > = HashMap::new();
    for pair in pairs {
        match api.get_positions(&pair).await {
            Ok(ps) => {
                exchange_positions.insert(pair, ps);
            }
            Err(e) => {
                // If we can't fetch positions, we cannot reconcile safely — fail fast
                // rather than risk duplicating close orders or leaving orphans.
                anyhow::bail!(
                    "startup reconcile: get_positions({pair}) failed for {}: {e}",
                    account.name
                );
            }
        }
    }

    for trade in &db_trades {
        let pair_positions = exchange_positions
            .get(&trade.pair.0)
            .cloned()
            .unwrap_or_default();
        let exchange_has_matching = pair_positions
            .iter()
            .any(|p| matches_direction(&p.side, &trade.direction) && p.size > Decimal::ZERO);

        match (trade.status.as_str(), exchange_has_matching) {
            ("open", true) => {
                tracing::info!(
                    "startup reconcile: trade {} consistent (DB=open, exchange=open)",
                    trade.id
                );
            }
            ("open", false) => {
                tracing::warn!(
                    "startup reconcile: trade {} DB=open but exchange has no matching position; \
                     closing with best-effort exit price",
                    trade.id
                );
                force_close_db_only(pool, trade, price_store, "startup_reconcile_orphan").await?;
            }
            ("closing", true) => {
                tracing::warn!(
                    "startup reconcile: trade {} DB=closing but exchange position still open — \
                     resetting to open for retry by normal close monitor",
                    trade.id
                );
                // Revert closing → open so the normal monitor loop will retry close.
                trades::release_close_lock(pool, trade.id).await?;
            }
            ("closing", false) => {
                tracing::warn!(
                    "startup reconcile: trade {} DB=closing and exchange shows no position — \
                     completing Phase 3 with best-effort exit price",
                    trade.id
                );
                force_close_db_only(pool, trade, price_store, "startup_reconcile_phase3").await?;
            }
            (other, _) => {
                // Should never happen — list_open_or_closing_by_account filters to
                // status IN ('open', 'closing'). Bail rather than silently skipping.
                anyhow::bail!(
                    "startup reconcile: unexpected status '{}' for trade {}",
                    other,
                    trade.id
                );
            }
        }
    }
    Ok(())
}

async fn force_close_db_only(
    pool: &PgPool,
    trade: &auto_trader_core::types::Trade,
    price_store: &PriceStore,
    reason_tag: &str,
) -> anyhow::Result<()> {
    // Best-effort exit price: PriceStore mid, fallback to entry_price.
    let feed_key = FeedKey::new(trade.exchange, trade.pair.clone());
    let exit_price = match price_store.latest_bid_ask(&feed_key).await {
        Some((bid, ask)) => (bid + ask) / Decimal::from(2),
        None => {
            tracing::warn!(
                "startup reconcile: no PriceStore data for {:?} {}; \
                 using entry_price as exit_price for trade {} (approximate)",
                trade.exchange,
                trade.pair,
                trade.id
            );
            trade.entry_price
        }
    };

    let pnl = match trade.direction {
        Direction::Long => (exit_price - trade.entry_price) * trade.quantity,
        Direction::Short => (trade.entry_price - exit_price) * trade.quantity,
    };
    // Truncate pnl to whole yen.
    let pnl = pnl.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);

    trades::close_trade_reconciled(pool, trade.id, exit_price, pnl, reason_tag).await?;
    Ok(())
}

fn resolve_exchange_enum(s: &str) -> anyhow::Result<auto_trader_core::types::Exchange> {
    match s {
        "bitflyer_cfd" => Ok(auto_trader_core::types::Exchange::BitflyerCfd),
        "oanda" => Ok(auto_trader_core::types::Exchange::Oanda),
        other => anyhow::bail!("unknown exchange: {}", other),
    }
}

fn matches_direction(side: &str, direction: &Direction) -> bool {
    let s = side.to_ascii_uppercase();
    match direction {
        Direction::Long => s == "BUY",
        Direction::Short => s == "SELL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::Direction;

    #[test]
    fn matches_direction_long() {
        assert!(matches_direction("BUY", &Direction::Long));
        assert!(matches_direction("buy", &Direction::Long));
        assert!(!matches_direction("SELL", &Direction::Long));
    }

    #[test]
    fn matches_direction_short() {
        assert!(matches_direction("SELL", &Direction::Short));
        assert!(matches_direction("sell", &Direction::Short));
        assert!(!matches_direction("BUY", &Direction::Short));
    }

    #[test]
    fn resolve_exchange_enum_known() {
        assert!(resolve_exchange_enum("bitflyer_cfd").is_ok());
        assert!(resolve_exchange_enum("oanda").is_ok());
    }

    #[test]
    fn resolve_exchange_enum_unknown() {
        assert!(resolve_exchange_enum("unknown_exchange").is_err());
    }
}
