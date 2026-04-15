//! Startup-time cross-check of config × env × DB accounts for live trading safety.

use auto_trader_core::config::LiveConfig;
use auto_trader_db::trading_accounts::TradingAccount;

/// Returns Err if any precondition for a safe start is violated.
///
/// `effective_dry_run` must be computed by the caller from the `LIVE_DRY_RUN`
/// env var and `[live].dry_run` config (env wins). Passing the already-resolved
/// value avoids a discrepancy where `LIVE_DRY_RUN=true` overrides `dry_run=false`
/// in the config but this validator still demands API keys.
pub fn validate_startup(
    accounts: &[TradingAccount],
    live_cfg: Option<&LiveConfig>,
    effective_dry_run: bool,
    slack_webhook_env: Option<&str>,
    bitflyer_key_env: Option<&str>,
    bitflyer_secret_env: Option<&str>,
) -> anyhow::Result<()> {
    let has_live = accounts.iter().any(|a| a.account_type == "live");
    let live_enabled = live_cfg.is_some_and(|l| l.enabled);

    // Fix 4: a single bitFlyer API client is a singleton — it cannot correctly
    // reconcile positions or sync balances across two distinct live exchange
    // accounts from one process. Reject ≥2 live accounts at startup.
    let live_count = accounts.iter().filter(|a| a.account_type == "live").count();
    if live_count > 1 {
        anyhow::bail!(
            "refusing to start: {} account_type='live' rows present; only 1 live account is supported per process (bitFlyer exchange client is singleton). If you need multiple live strategies, run them as separate processes.",
            live_count
        );
    }

    if has_live && !live_enabled {
        anyhow::bail!(
            "refusing to start: account_type='live' row(s) present in DB but [live].enabled is false (or [live] section missing). Set [live].enabled=true (and optionally [live].dry_run=true to force-simulate) before restarting."
        );
    }

    if live_enabled {
        // Fix 6: trim whitespace before empty-check so " " doesn't slip through.
        if slack_webhook_env.unwrap_or("").trim().is_empty() {
            anyhow::bail!(
                "refusing to start: [live].enabled=true requires SLACK_WEBHOOK_URL (critical events must be observable)"
            );
        }
        if !effective_dry_run {
            let key_empty = bitflyer_key_env.unwrap_or("").trim().is_empty();
            let secret_empty = bitflyer_secret_env.unwrap_or("").trim().is_empty();
            if key_empty || secret_empty {
                anyhow::bail!(
                    "refusing to start: [live].enabled=true with effective dry_run=false requires BITFLYER_API_KEY and BITFLYER_API_SECRET"
                );
            }
        }
    }
    Ok(())
}
