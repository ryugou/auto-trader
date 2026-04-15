use auto_trader::startup::validate_startup;
use auto_trader_core::config::LiveConfig;
use auto_trader_db::trading_accounts::TradingAccount;
use rust_decimal_macros::dec;
use uuid::Uuid;

fn live_account() -> TradingAccount {
    TradingAccount {
        id: Uuid::new_v4(),
        name: "live1".into(),
        account_type: "live".into(),
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
    LiveConfig {
        enabled,
        dry_run,
        execution_poll_interval_secs: 3,
        reconciler_interval_secs: 300,
        balance_sync_interval_secs: 300,
    }
}

#[test]
fn fails_when_live_account_exists_but_disabled() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(false, true)),
        Some("https://hook"),
        Some("k"),
        Some("s"),
    );
    assert!(r.is_err());
}

#[test]
fn fails_when_live_enabled_without_slack_webhook() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(true, true)),
        Some(""),
        Some("k"),
        Some("s"),
    );
    assert!(r.is_err());
}

#[test]
fn fails_when_real_trading_without_api_keys() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(true, false)),
        Some("https://hook"),
        Some(""),
        Some(""),
    );
    assert!(r.is_err());
}

#[test]
fn passes_when_live_dry_run_with_slack() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(true, true)),
        Some("https://hook"),
        None,
        None,
    );
    assert!(r.is_ok());
}

#[test]
fn passes_with_only_paper_accounts() {
    let mut a = live_account();
    a.account_type = "paper".into();
    let r = validate_startup(&[a], None, None, None, None);
    assert!(r.is_ok());
}
