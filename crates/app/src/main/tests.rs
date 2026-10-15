use auto_trader_db::trading_accounts::TradingAccount;
use rust_decimal_macros::dec;

use super::*;

fn sample_account(active: bool) -> TradingAccount {
    TradingAccount {
        id: uuid::Uuid::nil(),
        name: "test-account".to_string(),
        account_type: "paper".to_string(),
        exchange: "gmo_fx".to_string(),
        strategy: "donchian_trend".to_string(),
        initial_balance: dec!(100000),
        current_balance: dec!(100000),
        leverage: dec!(1),
        currency: "JPY".to_string(),
        created_at: chrono::Utc::now(),
        active,
    }
}

#[test]
fn account_still_tradable_true_for_active_account() {
    assert!(account_still_tradable(Some(&sample_account(true))));
}

#[test]
fn account_still_tradable_false_for_retired_account() {
    assert!(!account_still_tradable(Some(&sample_account(false))));
}

#[test]
fn account_still_tradable_false_when_account_missing() {
    assert!(!account_still_tradable(None));
}

#[test]
fn rejects_new_live_order_when_live_disabled() {
    assert!(live_order_blocked_when_live_disabled_leaving_close_paths_unaffected("live", false));
}

#[test]
fn allows_new_live_order_when_live_enabled() {
    assert!(!live_order_blocked_when_live_disabled_leaving_close_paths_unaffected("live", true));
}

#[test]
fn allows_new_paper_order_regardless_of_live_enabled() {
    assert!(!live_order_blocked_when_live_disabled_leaving_close_paths_unaffected("paper", false));
}

#[test]
fn overnight_fee_applies_to_bitflyer_cfd_and_gmo_fx() {
    assert!(overnight_fee_applies_to_exchange(Exchange::BitflyerCfd));
    assert!(overnight_fee_applies_to_exchange(Exchange::GmoFx));
}

#[test]
fn overnight_fee_does_not_apply_to_oanda() {
    assert!(!overnight_fee_applies_to_exchange(Exchange::Oanda));
}

#[test]
fn sfd_applies_only_to_fx_btc_jpy() {
    assert!(trade_is_subject_to_bitflyer_sfd("FX_BTC_JPY"));
    assert!(!trade_is_subject_to_bitflyer_sfd("ETH_JPY"));
}

#[test]
fn trade_not_yet_open_at_event_time_is_excluded_from_catchup_billing() {
    let event_at = chrono::Utc::now();
    let entry_after_event = event_at + chrono::Duration::minutes(1);
    let entry_before_event = event_at - chrono::Duration::minutes(1);
    assert!(trade_was_not_yet_open_at_event_time(
        entry_after_event,
        event_at
    ));
    assert!(!trade_was_not_yet_open_at_event_time(
        entry_before_event,
        event_at
    ));
}
