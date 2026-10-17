use super::*;

mod effective_dry_run_tests {
    use super::effective_dry_run;

    #[test]
    fn paper_is_always_dry_run() {
        assert!(effective_dry_run("paper", false));
        assert!(effective_dry_run("paper", true));
    }

    #[test]
    fn live_is_dry_run_only_when_forced() {
        assert!(!effective_dry_run("live", false));
        assert!(effective_dry_run("live", true));
    }
}

mod fx_new_dispatch_allowed_tests {
    use auto_trader_core::types::Exchange;

    use super::fx_new_dispatch_allowed;

    #[test]
    fn fx_new_strategy_on_gmo_fx_account_is_allowed() {
        assert!(fx_new_dispatch_allowed("fx_new_v1", Exchange::GmoFx));
    }

    #[test]
    fn fx_new_strategy_on_oanda_account_is_rejected() {
        assert!(!fx_new_dispatch_allowed("fx_new_v1", Exchange::Oanda));
    }

    #[test]
    fn fx_new_strategy_on_bitflyer_cfd_account_is_rejected() {
        assert!(!fx_new_dispatch_allowed("fx_new_v1", Exchange::BitflyerCfd));
    }

    #[test]
    fn non_fx_new_strategy_is_allowed_on_any_exchange() {
        assert!(fx_new_dispatch_allowed(
            "donchian_trend_v1",
            Exchange::Oanda
        ));
        assert!(fx_new_dispatch_allowed(
            "donchian_trend_v1",
            Exchange::BitflyerCfd
        ));
        assert!(fx_new_dispatch_allowed(
            "donchian_trend_v1",
            Exchange::GmoFx
        ));
    }
}

mod fx_new_account_assumption_tests {
    use auto_trader_db::trading_accounts::TradingAccount;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    use super::{FxNewMismatchKind, check_fx_new_account_assumptions};

    fn account(
        strategy: &str,
        exchange: &str,
        leverage: rust_decimal::Decimal,
        active: bool,
    ) -> TradingAccount {
        TradingAccount {
            id: Uuid::new_v4(),
            name: format!("{strategy}-account"),
            account_type: "paper".to_string(),
            exchange: exchange.to_string(),
            strategy: strategy.to_string(),
            initial_balance: dec!(100000),
            current_balance: dec!(100000),
            leverage,
            currency: "JPY".to_string(),
            created_at: Utc::now(),
            active,
        }
    }

    #[test]
    fn matching_leverage_and_exchange_on_active_fx_new_account_has_no_mismatch() {
        let accounts = vec![account("fx_new_v1", "gmo_fx", dec!(10), true)];
        assert!(check_fx_new_account_assumptions(&accounts).is_empty());
    }

    #[test]
    fn wrong_exchange_on_active_fx_new_account_is_reported_as_exchange_mismatch() {
        let accounts = vec![account("fx_new_v1", "oanda", dec!(10), true)];
        let mismatches = check_fx_new_account_assumptions(&accounts);
        assert_eq!(mismatches.len(), 1, "expected exactly one mismatch");
        let m = &mismatches[0];
        assert_eq!(m.account_name, "fx_new_v1-account");
        assert_eq!(m.account_exchange, "oanda");
        assert_eq!(
            m.kind,
            FxNewMismatchKind::Exchange {
                expected_exchange: auto_trader_strategy::fx_new::FX_NEW_EXCHANGE,
            }
        );
    }

    #[test]
    fn wrong_leverage_on_active_fx_new_account_is_reported_as_leverage_mismatch() {
        let accounts = vec![account("fx_new_v1", "gmo_fx", dec!(5), true)];
        let mismatches = check_fx_new_account_assumptions(&accounts);
        assert_eq!(mismatches.len(), 1, "expected exactly one mismatch");
        let m = &mismatches[0];
        assert_eq!(m.account_name, "fx_new_v1-account");
        assert_eq!(m.account_exchange, "gmo_fx");
        assert_eq!(
            m.kind,
            FxNewMismatchKind::Leverage {
                account_leverage: dec!(5),
                expected_leverage: dec!(10),
            }
        );
    }

    #[test]
    fn wrong_leverage_and_exchange_on_active_fx_new_account_reports_both_mismatches() {
        let accounts = vec![account("fx_new_v1", "oanda", dec!(5), true)];
        let mismatches = check_fx_new_account_assumptions(&accounts);
        assert_eq!(mismatches.len(), 2, "expected one mismatch per kind");
        assert!(
            mismatches.iter().any(|m| m.kind
                == FxNewMismatchKind::Leverage {
                    account_leverage: dec!(5),
                    expected_leverage: dec!(10),
                }),
            "missing leverage mismatch: {mismatches:?}"
        );
        assert!(
            mismatches.iter().any(|m| m.kind
                == FxNewMismatchKind::Exchange {
                    expected_exchange: auto_trader_strategy::fx_new::FX_NEW_EXCHANGE,
                }),
            "missing exchange mismatch: {mismatches:?}"
        );
    }

    #[test]
    fn unrecognised_exchange_string_on_active_fx_new_account_is_reported_with_raw_value() {
        let accounts = vec![account("fx_new_v1", "unknown_fx", dec!(10), true)];
        let mismatches = check_fx_new_account_assumptions(&accounts);
        assert_eq!(mismatches.len(), 1, "expected exactly one mismatch");
        let m = &mismatches[0];
        assert_eq!(
            m.account_exchange, "unknown_fx",
            "raw unparseable exchange string must be preserved verbatim"
        );
        assert_eq!(
            m.kind,
            FxNewMismatchKind::Exchange {
                expected_exchange: auto_trader_strategy::fx_new::FX_NEW_EXCHANGE,
            }
        );
    }

    #[test]
    fn mismatches_on_inactive_fx_new_account_are_ignored() {
        let accounts = vec![account("fx_new_v1", "oanda", dec!(5), false)];
        assert!(
            check_fx_new_account_assumptions(&accounts).is_empty(),
            "inactive accounts must not be flagged regardless of leverage/exchange"
        );
    }

    #[test]
    fn mismatches_on_non_fx_new_strategy_are_ignored() {
        let accounts = vec![account("donchian_trend_v1", "oanda", dec!(5), true)];
        assert!(
            check_fx_new_account_assumptions(&accounts).is_empty(),
            "strategies not prefixed with fx_new must not be checked"
        );
    }
}

mod resolve_exchange_liquidation_levels_tests {
    use auto_trader_core::config::AppConfig;
    use auto_trader_core::types::Exchange;
    use auto_trader_db::trading_accounts::TradingAccount;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    use super::resolve_exchange_liquidation_levels;

    fn minimal_config() -> AppConfig {
        serde_json::from_str(
            r#"{
                "vegapunk": {"endpoint": "http://x", "schema": "y"},
                "database": {"url": "postgresql://x"},
                "monitor": {"interval_secs": 60},
                "pairs": {}
            }"#,
        )
        .expect("minimal_config must parse")
    }

    fn config_with_margin(entries: &[(&str, &str)]) -> AppConfig {
        let margin_entries: String = entries
            .iter()
            .map(|(name, value)| format!(r#""{name}": {{"liquidation_margin_level": {value}}}"#))
            .collect::<Vec<_>>()
            .join(",");
        serde_json::from_str(&format!(
            r#"{{
                "vegapunk": {{"endpoint": "http://x", "schema": "y"}},
                "database": {{"url": "postgresql://x"}},
                "monitor": {{"interval_secs": 60}},
                "pairs": {{}},
                "exchange_margin": {{{margin_entries}}}
            }}"#
        ))
        .expect("config_with_margin must parse")
    }

    fn account_with_exchange(exchange: &str) -> TradingAccount {
        TradingAccount {
            id: Uuid::new_v4(),
            name: "acct".to_string(),
            account_type: "paper".to_string(),
            exchange: exchange.to_string(),
            strategy: "donchian_trend_v1".to_string(),
            initial_balance: dec!(100000),
            current_balance: dec!(100000),
            leverage: dec!(10),
            currency: "JPY".to_string(),
            created_at: Utc::now(),
            active: true,
        }
    }

    #[test]
    fn rejects_account_with_unparseable_exchange() {
        let accounts = vec![account_with_exchange("not_a_real_exchange")];
        let config = minimal_config();
        let err = resolve_exchange_liquidation_levels(&accounts, &config).unwrap_err();
        assert!(
            format!("{err}").contains("not_a_real_exchange"),
            "error must name the offending exchange value: {err}"
        );
    }

    #[test]
    fn rejects_non_positive_margin_for_exchange_in_use() {
        let accounts = vec![account_with_exchange("bitflyer_cfd")];
        let config = config_with_margin(&[("bitflyer_cfd", "0")]);
        let err = resolve_exchange_liquidation_levels(&accounts, &config).unwrap_err();
        let message = format!("{err}");
        assert!(
            message.contains("exchange_margin.bitflyer_cfd.liquidation_margin_level"),
            "error must name the offending config key: {message}"
        );
        assert!(
            message.contains("must be positive"),
            "error must state the reason: {message}"
        );
    }

    #[test]
    fn reports_all_missing_exchanges_sorted() {
        let accounts = vec![
            account_with_exchange("oanda"),
            account_with_exchange("bitflyer_cfd"),
        ];
        let config = config_with_margin(&[]);
        let err = resolve_exchange_liquidation_levels(&accounts, &config).unwrap_err();
        let message = format!("{err}");
        let bitflyer_pos = message.find("\"exchange_margin.bitflyer_cfd\"");
        let oanda_pos = message.find("\"exchange_margin.oanda\"");
        assert!(
            bitflyer_pos.is_some() && oanda_pos.is_some(),
            "error must name both missing exchanges: {message}"
        );
        assert!(
            bitflyer_pos < oanda_pos,
            "missing exchanges must be sorted: {message}"
        );
    }

    #[test]
    fn returns_levels_for_exchanges_in_use() {
        let accounts = vec![account_with_exchange("bitflyer_cfd")];
        let config = config_with_margin(&[("bitflyer_cfd", "50")]);
        let map = resolve_exchange_liquidation_levels(&accounts, &config).unwrap();
        assert_eq!(map[&Exchange::BitflyerCfd], dec!(50));
    }
}
