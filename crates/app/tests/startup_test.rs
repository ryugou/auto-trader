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
        /*effective_dry_run=*/ true,
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
        /*effective_dry_run=*/ true,
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
        /*effective_dry_run=*/ false,
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
        /*effective_dry_run=*/ true,
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
    let r = validate_startup(
        &[a],
        None,
        /*effective_dry_run=*/ false,
        None,
        None,
        None,
    );
    assert!(r.is_ok());
}

// Fix 4: ≥2 live accounts must be rejected.
#[test]
fn fails_when_two_live_accounts_present() {
    let a1 = live_account();
    let mut a2 = live_account();
    a2.id = uuid::Uuid::new_v4();
    a2.name = "live2".into();
    let r = validate_startup(
        &[a1, a2],
        Some(&live_cfg(true, true)),
        /*effective_dry_run=*/ true,
        Some("https://hook"),
        None,
        None,
    );
    assert!(r.is_err());
    let msg = r.unwrap_err().to_string();
    assert!(
        msg.contains("2 account_type='live'"),
        "unexpected error: {msg}"
    );
}

// Fix 5: effective_dry_run=true from env must skip API key requirement even
// when [live].dry_run=false in config.
#[test]
fn passes_when_env_forces_dry_run_overriding_config() {
    // Config says dry_run=false, but caller already resolved env override → true.
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(true, false)),
        /*effective_dry_run=*/ true,
        Some("https://hook"),
        None, // no API keys
        None,
    );
    assert!(r.is_ok());
}

// Fix 6: whitespace-only SLACK_WEBHOOK_URL must be rejected.
#[test]
fn fails_when_slack_webhook_is_whitespace_only() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(true, true)),
        /*effective_dry_run=*/ true,
        Some("   "),
        None,
        None,
    );
    assert!(r.is_err());
}

// Fix 6: whitespace-only API keys must be rejected.
#[test]
fn fails_when_api_keys_are_whitespace_only() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(true, false)),
        /*effective_dry_run=*/ false,
        Some("https://hook"),
        Some("  "),
        Some("\t"),
    );
    assert!(r.is_err());
}
