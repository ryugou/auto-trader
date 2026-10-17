use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::types::Exchange;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_notify::Notifier;

pub(crate) async fn reconcile(
    pool: &sqlx::PgPool,
    db_accounts: &[auto_trader_db::trading_accounts::TradingAccount],
    exchange_apis: &HashMap<Exchange, Arc<dyn ExchangeApi>>,
    price_store: Arc<auto_trader::price_store::PriceStore>,
    notifier: Arc<Notifier>,
) -> anyhow::Result<()> {
    crate::startup_reconcile::reconcile_live_accounts_at_startup(
        pool,
        db_accounts,
        exchange_apis,
        price_store,
        notifier,
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "failed to reconcile live accounts at startup: {e}; refusing to start with potentially inconsistent state"
        )
    })
}
