use auto_trader_db::trading_accounts::TradingAccount;
use rust_decimal_macros::dec;

use super::*;

fn account(exchange: &str, account_type: &str) -> TradingAccount {
    TradingAccount {
        id: uuid::Uuid::nil(),
        name: "test-account".to_string(),
        account_type: account_type.to_string(),
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
fn has_gmo_paper_account_is_true_only_when_a_gmo_fx_paper_account_is_present() {
    let cases: Vec<(&str, Vec<TradingAccount>, bool)> = vec![
        (
            "gmo_fx_paper_account_is_true",
            vec![account("gmo_fx", "paper")],
            true,
        ),
        (
            "gmo_fx_live_account_only_is_false",
            vec![account("gmo_fx", "live")],
            false,
        ),
        (
            "oanda_paper_account_only_is_false",
            vec![account("oanda", "paper")],
            false,
        ),
        ("empty_account_list_is_false", vec![], false),
    ];

    for (name, accounts, expected) in cases {
        let actual = has_gmo_paper_account(&accounts);

        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn is_registered_is_true_only_when_strategy_is_in_registered_list() {
    let cases: Vec<(&str, Vec<String>, &str, bool)> = vec![
        (
            "strategy_present_in_registered_list_is_true",
            vec!["donchian_trend".to_string()],
            "donchian_trend",
            true,
        ),
        (
            "strategy_absent_from_registered_list_is_false",
            vec!["donchian_trend".to_string()],
            "swing_llm",
            false,
        ),
        (
            "empty_registered_list_is_false",
            vec![],
            "donchian_trend",
            false,
        ),
    ];

    for (name, registered, strategy, expected) in cases {
        let actual = is_registered(&registered, strategy);

        assert_eq!(actual, expected, "{name}");
    }
}
