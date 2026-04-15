//! Startup-time safety gate: bail if a live account exists but [live].enabled=false.

use auto_trader_core::config::LiveConfig;
use auto_trader_db::trading_accounts::TradingAccount;

/// Returns Err if a `account_type='live'` row is present in the DB but
/// `[live].enabled` is false (or the `[live]` config section is missing
/// entirely). All other combinations are allowed.
pub fn check_live_gate(
    accounts: &[TradingAccount],
    live_cfg: Option<&LiveConfig>,
) -> anyhow::Result<()> {
    let has_live = accounts.iter().any(|a| a.account_type == "live");
    let live_enabled = live_cfg.is_some_and(|l| l.enabled);

    if has_live && !live_enabled {
        anyhow::bail!(
            "refusing to start: account_type='live' row(s) present in DB but [live].enabled is \
             false (or [live] section missing). Set [live].enabled=true before restarting."
        );
    }
    Ok(())
}
