//! Startup-time cross-check of config × env × DB accounts for live trading safety.

use auto_trader_core::config::LiveConfig;
use auto_trader_db::trading_accounts::TradingAccount;
use std::collections::HashSet;
use uuid::Uuid;

/// Returns true if this account is allowed to execute against the live
/// exchange right now. Paper accounts are always allowed (no external
/// side-effect). Live accounts are allowed ONLY if their UUID was in the
/// startup-approved set. This is the single source of truth — every site
/// that would call `send_child_order` (or equivalent real-money side effect)
/// must consult this helper.
pub fn is_account_approved_for_execution(
    account_type: &str,
    account_id: Uuid,
    approved_live: &HashSet<Uuid>,
) -> bool {
    match account_type {
        "paper" => true,
        "live" => approved_live.contains(&account_id),
        _ => false, // future-proof: unknown account_type → refuse
    }
}

/// Parse `LIVE_DRY_RUN` env var against `[live].dry_run` config. Returns the
/// effective dry-run flag. Invalid env value logs a warning and falls back
/// to the config value. Both call sites (startup validation + executor
/// runtime dispatch) must see the same result.
pub fn resolve_effective_dry_run(live_cfg_dry_run: bool, env: Option<&str>) -> bool {
    match env {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            other => {
                tracing::warn!(
                    "ignoring invalid LIVE_DRY_RUN='{}' (expected true/false); falling back to [live].dry_run={}",
                    other,
                    live_cfg_dry_run
                );
                live_cfg_dry_run
            }
        },
        None => live_cfg_dry_run,
    }
}

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

#[cfg(test)]
mod tests {
    use super::{is_account_approved_for_execution, resolve_effective_dry_run};
    use std::collections::HashSet;
    use uuid::Uuid;

    #[test]
    fn paper_account_is_always_approved() {
        let approved: HashSet<Uuid> = HashSet::new();
        let id = Uuid::new_v4();
        assert!(is_account_approved_for_execution("paper", id, &approved));
    }

    #[test]
    fn live_account_in_set_is_approved() {
        let id = Uuid::new_v4();
        let approved: HashSet<Uuid> = [id].iter().cloned().collect();
        assert!(is_account_approved_for_execution("live", id, &approved));
    }

    #[test]
    fn live_account_not_in_set_is_refused() {
        let approved: HashSet<Uuid> = HashSet::new();
        let id = Uuid::new_v4();
        assert!(!is_account_approved_for_execution("live", id, &approved));
    }

    #[test]
    fn unknown_account_type_is_refused() {
        let id = Uuid::new_v4();
        let approved: HashSet<Uuid> = [id].iter().cloned().collect();
        assert!(!is_account_approved_for_execution("demo", id, &approved));
        assert!(!is_account_approved_for_execution("", id, &approved));
    }

    #[test]
    fn env_true_overrides_config_false() {
        assert!(resolve_effective_dry_run(false, Some("true")));
        assert!(resolve_effective_dry_run(false, Some("1")));
        assert!(resolve_effective_dry_run(false, Some("yes")));
        assert!(resolve_effective_dry_run(false, Some("on")));
    }

    #[test]
    fn env_false_overrides_config_true() {
        assert!(!resolve_effective_dry_run(true, Some("false")));
        assert!(!resolve_effective_dry_run(true, Some("0")));
        assert!(!resolve_effective_dry_run(true, Some("no")));
        assert!(!resolve_effective_dry_run(true, Some("off")));
    }

    #[test]
    fn invalid_env_falls_back_to_config() {
        // Invalid value should fall back to config value (true).
        assert!(resolve_effective_dry_run(true, Some("garbage")));
        // Invalid value should fall back to config value (false).
        assert!(!resolve_effective_dry_run(false, Some("garbage")));
    }

    #[test]
    fn no_env_uses_config() {
        assert!(resolve_effective_dry_run(true, None));
        assert!(!resolve_effective_dry_run(false, None));
    }
}
