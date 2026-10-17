use rust_decimal_macros::dec;

use super::*;

fn account_with_exchange(exchange: &str) -> TradingAccount {
    TradingAccount {
        id: uuid::Uuid::nil(),
        name: "test-account".to_string(),
        account_type: "paper".to_string(),
        exchange: exchange.to_string(),
        strategy: "donchian_trend".to_string(),
        initial_balance: dec!(100000),
        current_balance: dec!(100000),
        leverage: dec!(1),
        currency: "JPY".to_string(),
        created_at: chrono::Utc::now(),
        active: true,
    }
}

#[test]
fn resolve_account_exchange_maps_supported_exchanges_and_rejects_others() {
    let cases = [
        ("oanda", "oanda", Some(Exchange::Oanda)),
        ("bitflyer_cfd", "bitflyer_cfd", Some(Exchange::BitflyerCfd)),
        ("gmo_fx", "gmo_fx", Some(Exchange::GmoFx)),
        ("unknown", "binance", None),
        ("empty", "", None),
    ];

    for (name, exchange, expected) in cases {
        let actual = resolve_account_exchange(&account_with_exchange(exchange));

        assert_eq!(actual, expected, "{name}");
    }
}
