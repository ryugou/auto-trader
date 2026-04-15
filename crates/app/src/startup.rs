//! Startup-time cross-check of config × env × DB accounts for live trading safety.

use auto_trader_core::config::LiveConfig;
use auto_trader_db::trading_accounts::TradingAccount;

/// Returns Err if any precondition for a safe start is violated.
pub fn validate_startup(
    accounts: &[TradingAccount],
    live_cfg: Option<&LiveConfig>,
    slack_webhook_env: Option<&str>,
    bitflyer_key_env: Option<&str>,
    bitflyer_secret_env: Option<&str>,
) -> anyhow::Result<()> {
    let has_live = accounts.iter().any(|a| a.account_type == "live");
    let live_enabled = live_cfg.is_some_and(|l| l.enabled);
    let dry_run = live_cfg.is_some_and(|l| l.dry_run);

    if has_live && !live_enabled {
        anyhow::bail!(
            "refusing to start: account_type='live' row(s) present in DB but [live].enabled is false (or [live] section missing). Set [live].enabled=true (and optionally [live].dry_run=true to force-simulate) before restarting."
        );
    }

    if live_enabled {
        if slack_webhook_env.unwrap_or("").is_empty() {
            anyhow::bail!(
                "refusing to start: [live].enabled=true requires SLACK_WEBHOOK_URL (critical events must be observable)"
            );
        }
        if !dry_run {
            let key_empty = bitflyer_key_env.unwrap_or("").is_empty();
            let secret_empty = bitflyer_secret_env.unwrap_or("").is_empty();
            if key_empty || secret_empty {
                anyhow::bail!(
                    "refusing to start: [live].enabled=true with dry_run=false requires BITFLYER_API_KEY and BITFLYER_API_SECRET"
                );
            }
        }
    }
    Ok(())
}
