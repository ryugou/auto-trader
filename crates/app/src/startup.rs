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

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::config::LiveConfig;
    use auto_trader_db::trading_accounts::TradingAccount;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    fn account(account_type: &str) -> TradingAccount {
        TradingAccount {
            id: Uuid::new_v4(),
            name: "test".into(),
            account_type: account_type.into(),
            exchange: "bitflyer_cfd".into(),
            strategy: "donchian_trend_v1".into(),
            initial_balance: dec!(30000),
            current_balance: dec!(30000),
            leverage: dec!(2),
            currency: "JPY".into(),
            created_at: chrono::Utc::now(),
        }
    }

    fn live_cfg(enabled: bool, dry_run: bool) -> LiveConfig {
        LiveConfig { enabled, dry_run }
    }

    #[test]
    fn fails_when_live_account_present_but_disabled() {
        let accounts = vec![account("live")];
        let r = check_live_gate(&accounts, Some(&live_cfg(false, true)));
        assert!(r.is_err());
    }

    #[test]
    fn passes_with_no_live_accounts_regardless_of_config() {
        let accounts = vec![account("paper")];
        assert!(check_live_gate(&accounts, None).is_ok());
        assert!(check_live_gate(&accounts, Some(&live_cfg(false, true))).is_ok());
        assert!(check_live_gate(&accounts, Some(&live_cfg(true, false))).is_ok());
    }

    #[test]
    fn passes_when_live_present_and_enabled() {
        let accounts = vec![account("live")];
        let r = check_live_gate(&accounts, Some(&live_cfg(true, true)));
        assert!(r.is_ok());
    }
}
