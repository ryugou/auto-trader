use auto_trader_core::types::Exchange;
use rust_decimal_macros::dec;

use super::*;

#[test]
fn resolve_notify_exchange_maps_known_exchanges_and_falls_back_to_fx_new_exchange() {
    let cases = [
        ("oanda_maps_to_oanda", "oanda", Exchange::Oanda),
        ("gmo_fx_maps_to_gmo_fx", "gmo_fx", Exchange::GmoFx),
        (
            "unknown_exchange_falls_back_to_fx_new_exchange",
            "unknown",
            auto_trader_strategy::fx_new::FX_NEW_EXCHANGE,
        ),
        (
            "empty_exchange_falls_back_to_fx_new_exchange",
            "",
            auto_trader_strategy::fx_new::FX_NEW_EXCHANGE,
        ),
    ];

    for (name, account_exchange, expected) in cases {
        let actual = resolve_notify_exchange(account_exchange);

        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn format_leverage_mismatch_body_states_account_and_both_leverages() {
    let body = format_leverage_mismatch_body("acct-1", &dec!(5), &dec!(1));

    assert_eq!(
        body,
        "account 'acct-1' has leverage 5 but fx_new strategy assumes 1 — risk sizing (1% of equity at stop) is wrong for this account until leverage is corrected"
    );
}

#[test]
fn format_exchange_mismatch_body_states_account_and_both_exchanges() {
    let body = format_exchange_mismatch_body("acct-1", "oanda", Exchange::GmoFx);

    assert_eq!(
        body,
        "account 'acct-1' has exchange 'oanda' but fx_new only processes gmo_fx events — the executor will not dispatch fx_new signals to this account until the exchange is corrected, so no order will be placed for it"
    );
}
