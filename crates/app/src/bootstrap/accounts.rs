use auto_trader_core::config::GmoFxSwapConfig;
use auto_trader_db::trading_accounts::TradingAccount;

pub(crate) async fn load_and_log(
    pool: &sqlx::PgPool,
    registered_strategies: &[String],
    gmo_fx_swap: &GmoFxSwapConfig,
) -> anyhow::Result<Vec<TradingAccount>> {
    let db_accounts = match auto_trader_db::trading_accounts::list_active_or_with_open_trades(pool)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            anyhow::bail!(
                "failed to list trading accounts at startup: {e}; cannot validate live-trading preconditions, refusing to start"
            );
        }
    };
    if db_accounts.is_empty() {
        tracing::info!("no trading accounts found in DB at startup");
    }
    for pac in &db_accounts {
        log_account(pac, registered_strategies);
    }

    log_swap_freshness_at_startup_leaving_slack_to_the_daily_job(
        gmo_fx_swap,
        chrono::Utc::now().date_naive(),
        has_gmo_paper_account(&db_accounts),
    );

    Ok(db_accounts)
}

fn log_swap_freshness_at_startup_leaving_slack_to_the_daily_job(
    gmo_fx_swap_config: &GmoFxSwapConfig,
    today: chrono::NaiveDate,
    has_gmo_paper_accounts: bool,
) {
    if let Some(body) = auto_trader::swap_freshness::swap_freshness_alert(
        gmo_fx_swap_config,
        today,
        has_gmo_paper_accounts,
    ) {
        tracing::warn!(body = %body, "invalid swap config: stale");
    }
}

fn log_account(pac: &TradingAccount, registered_strategies: &[String]) {
    tracing::info!(
        account_id = %pac.id,
        account_name = %pac.name,
        account_type = %pac.account_type,
        exchange = %pac.exchange,
        strategy_name = %pac.strategy,
        current_balance = %pac.current_balance,
        initial_balance = %pac.initial_balance,
        leverage = %pac.leverage,
        active = pac.active,
        "found trading account at startup"
    );
    if !is_registered(registered_strategies, &pac.strategy) {
        tracing::warn!(
            account_name = %pac.name,
            strategy_name = %pac.strategy,
            "invalid strategy: not registered"
        );
    }
}

fn has_gmo_paper_account(accounts: &[TradingAccount]) -> bool {
    accounts
        .iter()
        .any(|a| a.exchange == "gmo_fx" && a.account_type == "paper")
}

fn is_registered(registered_strategies: &[String], strategy: &str) -> bool {
    registered_strategies.iter().any(|s| s == strategy)
}

#[cfg(test)]
mod tests;
