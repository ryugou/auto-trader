use auto_trader::balance_drift::BalanceDriftContext;
use auto_trader_db::trading_accounts::TradingAccount;

use super::live_accounts;
use crate::runtime::balance_drift_loop;
use crate::runtime::dependencies::SharedDependencies;

pub(crate) async fn run(
    dependencies: &SharedDependencies,
    db_accounts: &[TradingAccount],
    live_forces_dry_run: bool,
) -> anyhow::Result<BalanceDriftContext> {
    live_accounts::reconcile(
        &dependencies.pool,
        db_accounts,
        &dependencies.exchange_apis,
        dependencies.price_store.clone(),
        dependencies.notifier.clone(),
    )
    .await?;

    let drift_ctx = BalanceDriftContext {
        pool: dependencies.pool.clone(),
        price_store: dependencies.price_store.clone(),
        apis: dependencies.exchange_apis.clone(),
        live_forces_dry_run,
    };
    balance_drift_loop::check_and_alert(&drift_ctx, &dependencies.notifier).await;

    Ok(drift_ctx)
}
