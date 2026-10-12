use super::*;

mod debug_redaction_tests {
    use super::*;

    #[test]
    fn bitflyer_config_debug_redacts_api_key_and_secret() {
        let cfg = BitflyerConfig {
            ws_url: "wss://example".to_string(),
            api_url: "https://example".to_string(),
            api_key: Some("AKIA_SHOULD_NEVER_APPEAR".to_string()),
            api_secret: Some("SUPER_SECRET_SHOULD_NEVER_APPEAR".to_string()),
        };
        let rendered = format!("{cfg:?}");
        assert!(
            !rendered.contains("AKIA_SHOULD_NEVER_APPEAR"),
            "api_key leaked: {rendered}"
        );
        assert!(
            !rendered.contains("SUPER_SECRET_SHOULD_NEVER_APPEAR"),
            "api_secret leaked: {rendered}"
        );
        assert!(rendered.contains("redacted"));
        assert!(rendered.contains("wss://example"));
    }

    #[test]
    fn bitflyer_config_debug_shows_none_when_unset() {
        let cfg = BitflyerConfig {
            ws_url: "wss://example".to_string(),
            api_url: "https://example".to_string(),
            api_key: None,
            api_secret: None,
        };
        let rendered = format!("{cfg:?}");
        assert!(rendered.contains("api_key: None"));
        assert!(rendered.contains("api_secret: None"));
    }
}

mod jpy_quote_validation_tests {
    use super::*;

    fn base_toml(pairs_fx: &str, strategy_pairs: &str) -> String {
        format!(
            r#"
[vegapunk]
endpoint = "http://localhost:6840"
schema = "fx-trading"

[database]
url = "postgresql://localhost/test"

[monitor]
interval_secs = 60

[pairs]
fx = {pairs_fx}
crypto = ["FX_BTC_JPY"]

[[strategies]]
name = "donchian_trend_v1"
enabled = true
mode = "paper"
pairs = {strategy_pairs}
"#
        )
    }

    #[test]
    fn rejects_non_jpy_quote_pair_in_pairs_fx() {
        let toml_str = base_toml(r#"["USD_JPY", "EUR_USD"]"#, r#"["USD_JPY"]"#);
        let config: AppConfig = toml::from_str(&toml_str).unwrap();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("EUR_USD"), "error should name the pair: {err}");
    }

    #[test]
    fn rejects_non_jpy_quote_pair_in_strategy_pairs() {
        let toml_str = base_toml(r#"["USD_JPY"]"#, r#"["EUR_USD"]"#);
        let config: AppConfig = toml::from_str(&toml_str).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_jpy_quote_pairs() {
        let toml_str = base_toml(r#"["USD_JPY"]"#, r#"["USD_JPY", "FX_BTC_JPY"]"#);
        let config: AppConfig = toml::from_str(&toml_str).unwrap();
        assert!(config.validate().is_ok());
    }
}

mod validation_tests {
    use super::*;

    fn valid_live() -> LiveConfig {
        LiveConfig {
            enabled: false,
            dry_run: true,
        }
    }

    #[test]
    fn live_validate_accepts_valid_values() {
        valid_live().validate().unwrap();
    }
}

#[test]
fn parse_minimal_config() {
    let toml_str = r#"
[oanda]
api_url = "https://api-fxpractice.oanda.com"
account_id = "101-001-12345678-001"

[vegapunk]
endpoint = "http://fuj11-agent-01:3000"
schema = "fx-trading"

[database]
url = "postgresql://user:pass@localhost:5432/auto_trader"

[monitor]
interval_secs = 60

[pairs]
active = ["USD_JPY"]

[[strategies]]
name = "swing_llm_v1"
enabled = false
mode = "paper"
pairs = ["USD_JPY"]
params = { holding_days_max = 14 }
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(
        config.oanda.as_ref().unwrap().api_url,
        "https://api-fxpractice.oanda.com"
    );
    assert_eq!(config.strategies.len(), 1);
    assert_eq!(config.strategies[0].name, "swing_llm_v1");
    assert_eq!(config.pairs.active, vec!["USD_JPY"]);
}

#[test]
fn parse_config_with_crypto() {
    let toml_str = r#"
[oanda]
api_url = "https://api-fxpractice.oanda.com"
account_id = "101-001-12345678-001"

[bitflyer]
ws_url = "wss://ws.lightstream.bitflyer.com/json-rpc"
api_url = "https://api.bitflyer.com"

[vegapunk]
endpoint = "http://localhost:3000"
schema = "fx-trading"

[database]
url = "postgresql://user:pass@localhost:5432/auto_trader"

[monitor]
interval_secs = 60

[pairs]
fx = ["USD_JPY"]
crypto = ["FX_BTC_JPY"]

[pair_config.FX_BTC_JPY]
price_unit = 1
min_order_size = 0.001

[pair_config.USD_JPY]
price_unit = 0.001
min_order_size = 1

[position_sizing]
method = "risk_based"
risk_rate = 0.02

[[strategies]]
name = "donchian_trend_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY"]
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(
        config.bitflyer.as_ref().unwrap().ws_url,
        "wss://ws.lightstream.bitflyer.com/json-rpc"
    );
    assert_eq!(config.pairs.crypto.as_ref().unwrap().len(), 1);
    assert_eq!(
        config
            .pair_config
            .get("FX_BTC_JPY")
            .unwrap()
            .price_unit
            .to_string(),
        "1"
    );
    assert_eq!(
        config
            .position_sizing
            .as_ref()
            .unwrap()
            .risk_rate
            .to_string(),
        "0.02"
    );
}

#[test]
fn parse_config_with_live() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://localhost:3000"
schema = "fx-trading"

[database]
url = "postgresql://u:p@localhost/auto_trader"

[monitor]
interval_secs = 60

[pairs]
active = ["USD_JPY"]

[live]
enabled = false
dry_run = true
"#;
    let cfg: AppConfig = toml::from_str(toml_str).unwrap();
    let live = cfg.live.expect("live section should parse");
    assert!(!live.enabled);
    assert!(live.dry_run);
}

#[test]
fn parse_config_with_risk() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://localhost:3000"
schema = "fx-trading"

[database]
url = "postgresql://u:p@localhost/auto_trader"

[monitor]
interval_secs = 60

[pairs]
active = ["USD_JPY"]

[risk]
price_freshness_secs = 60
"#;
    let cfg: AppConfig = toml::from_str(toml_str).unwrap();
    let risk = cfg.risk.expect("risk section should parse");
    assert_eq!(risk.price_freshness_secs, 60);
    risk.validate().unwrap();
}

#[test]
fn risk_validate_rejects_zero_freshness() {
    let r = crate::config::RiskConfig {
        price_freshness_secs: 0,
        daily_loss_limit_pct: rust_decimal_macros::dec!(0.05),
        halt_hours: 24,
        sizing_margin_buffer: rust_decimal_macros::dec!(0.10),
    };
    assert!(r.validate().is_err());
}

#[test]
fn risk_defaults_apply_when_kill_switch_fields_omitted() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://localhost:3000"
schema = "fx-trading"

[database]
url = "postgresql://u:p@localhost/auto_trader"

[monitor]
interval_secs = 60

[pairs]
active = ["USD_JPY"]

[risk]
price_freshness_secs = 60
"#;
    let cfg: AppConfig = toml::from_str(toml_str).unwrap();
    let risk = cfg.risk.expect("risk section should parse");
    assert_eq!(risk.daily_loss_limit_pct, rust_decimal_macros::dec!(0.05));
    assert_eq!(risk.halt_hours, 24);
    risk.validate().unwrap();
}

#[test]
fn risk_config_default_matches_serde_defaults() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://localhost:3000"
schema = "fx-trading"

[database]
url = "postgresql://u:p@localhost/auto_trader"

[monitor]
interval_secs = 60

[pairs]
active = ["USD_JPY"]

[risk]
price_freshness_secs = 60
"#;
    let cfg: AppConfig = toml::from_str(toml_str).unwrap();
    let from_serde = cfg.risk.expect("risk section should parse");
    let default = RiskConfig::default();
    assert_eq!(
        default.price_freshness_secs,
        from_serde.price_freshness_secs
    );
    assert_eq!(
        default.daily_loss_limit_pct,
        from_serde.daily_loss_limit_pct
    );
    assert_eq!(default.halt_hours, from_serde.halt_hours);
    assert_eq!(
        default.sizing_margin_buffer,
        from_serde.sizing_margin_buffer
    );
}

#[test]
fn risk_validate_rejects_out_of_range_loss_pct() {
    let too_big = crate::config::RiskConfig {
        price_freshness_secs: 60,
        daily_loss_limit_pct: rust_decimal_macros::dec!(1),
        halt_hours: 24,
        sizing_margin_buffer: rust_decimal_macros::dec!(0.10),
    };
    assert!(too_big.validate().is_err());
    let zero = crate::config::RiskConfig {
        price_freshness_secs: 60,
        daily_loss_limit_pct: rust_decimal_macros::dec!(0),
        halt_hours: 24,
        sizing_margin_buffer: rust_decimal_macros::dec!(0.10),
    };
    assert!(zero.validate().is_err());
}

#[test]
fn risk_validate_rejects_zero_halt_hours() {
    let r = crate::config::RiskConfig {
        price_freshness_secs: 60,
        daily_loss_limit_pct: rust_decimal_macros::dec!(0.05),
        halt_hours: 0,
        sizing_margin_buffer: rust_decimal_macros::dec!(0.10),
    };
    assert!(r.validate().is_err());
}

#[test]
fn bitflyer_config_api_key_starts_as_none() {
    let toml_str = r#"
[vegapunk]
endpoint = "x"
schema = "x"
[database]
url = "x"
[monitor]
interval_secs = 1
[pairs]
active = []
[bitflyer]
ws_url = "wss://example"
api_url = "https://example"
api_key = "LEAKED"
api_secret = "LEAKED"
"#;
    let cfg: AppConfig = toml::from_str(toml_str).unwrap();
    let bf = cfg.bitflyer.unwrap();
    assert_eq!(bf.ws_url, "wss://example");
    assert!(bf.api_key.is_none(), "api_key must only come from env");
    assert!(
        bf.api_secret.is_none(),
        "api_secret must only come from env"
    );
}

#[test]
fn parses_exchange_margin_section() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = ["USD_JPY"]
crypto = ["FX_BTC_JPY"]

[exchange_margin.bitflyer_cfd]
liquidation_margin_level = 0.50

[exchange_margin.gmo_fx]
liquidation_margin_level = 1.00
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(
        config
            .exchange_margin
            .get("bitflyer_cfd")
            .map(|c| c.liquidation_margin_level),
        Some(rust_decimal_macros::dec!(0.50))
    );
    assert_eq!(
        config
            .exchange_margin
            .get("gmo_fx")
            .map(|c| c.liquidation_margin_level),
        Some(rust_decimal_macros::dec!(1.00))
    );
}

#[test]
fn exchange_margin_defaults_to_empty_when_missing() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = []
crypto = []
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert!(config.exchange_margin.is_empty());
}

#[test]
fn parses_gmo_fx_swap_section() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = []
crypto = []

[gmo_fx.swap]
updated_on = "2026-07-07"

[gmo_fx.swap.rates]
USD_JPY = { long = 100, short = -120 }
EUR_JPY = { long = 80, short = -100 }
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    let rates = &config.gmo_fx.swap.rates;
    assert_eq!(
        rates.get("USD_JPY").unwrap().long,
        rust_decimal_macros::dec!(100)
    );
    assert_eq!(
        rates.get("USD_JPY").unwrap().short,
        rust_decimal_macros::dec!(-120)
    );
    assert_eq!(
        rates.get("EUR_JPY").unwrap().long,
        rust_decimal_macros::dec!(80)
    );
    assert!(rates.get("GBP_JPY").is_none());
}

#[test]
fn gmo_fx_swap_defaults_to_empty_when_missing() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = []
crypto = []
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert!(config.gmo_fx.swap.rates.is_empty());
}

#[test]
fn gmo_fx_swap_max_age_days_defaults_to_monthly_update_plus_grace() {
    assert_eq!(GmoFxSwapConfig::default().max_age_days, 35);
}

#[test]
fn swap_rates_without_updated_on_fail_validation() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = []
crypto = []

[gmo_fx.swap.rates]
USD_JPY = { long = 100, short = -120 }
EUR_JPY = { long = 80, short = -100 }
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert!(config.validate().is_err());
}

#[test]
fn swap_updated_on_must_be_valid_date() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = []
crypto = []

[gmo_fx.swap]
updated_on = "not-a-date"

[gmo_fx.swap.rates]
USD_JPY = { long = 100, short = -120 }
EUR_JPY = { long = 80, short = -100 }
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert!(config.validate().is_err());
}

#[test]
fn empty_rates_do_not_require_updated_on() {
    let toml_str = r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = []
crypto = []
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert!(config.validate().is_ok());
}

mod feed_watchdog_config_tests {
    use super::*;

    fn minimal_config_toml() -> String {
        r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = []
crypto = []
"#
        .to_string()
    }

    #[test]
    fn section_absent_uses_issue_109_defaults() {
        let config: AppConfig = toml::from_str(&minimal_config_toml()).unwrap();
        assert!(config.validate().is_ok());
        let fw = config.feed_watchdog;
        assert!(fw.enabled);
        assert_eq!(fw.check_interval_secs, 60);
        assert_eq!(fw.suspend_gap_secs, 300);
        assert_eq!(fw.stale_after_secs, 900);
        assert_eq!(fw.realert_interval_secs, 3600);
        assert_eq!(fw.exclude_exchanges, vec!["oanda".to_string()]);
        assert_eq!(fw.notify_retry_attempts, 5);
        assert_eq!(fw.notify_retry_initial_secs, 30);
    }

    #[test]
    fn partial_section_fills_missing_keys_with_defaults() {
        let toml_str = format!(
            "{}\n[feed_watchdog]\nenabled = false\n",
            minimal_config_toml()
        );
        let config: AppConfig = toml::from_str(&toml_str).unwrap();
        let fw = config.feed_watchdog;
        assert!(!fw.enabled);
        assert_eq!(fw.check_interval_secs, 60);
        assert_eq!(fw.suspend_gap_secs, 300);
        assert_eq!(fw.stale_after_secs, 900);
        assert_eq!(fw.realert_interval_secs, 3600);
        assert_eq!(fw.exclude_exchanges, vec!["oanda".to_string()]);
        assert_eq!(fw.notify_retry_attempts, 5);
        assert_eq!(fw.notify_retry_initial_secs, 30);
    }

    #[test]
    fn explicit_values_parse() {
        let toml_str = format!(
            "{}\n[feed_watchdog]\nenabled = true\ncheck_interval_secs = 30\n\
             suspend_gap_secs = 120\nstale_after_secs = 90\nrealert_interval_secs = 1800\n\
             notify_retry_attempts = 3\nnotify_retry_initial_secs = 10\n",
            minimal_config_toml()
        );
        let config: AppConfig = toml::from_str(&toml_str).unwrap();
        config.validate().unwrap();
        let fw = config.feed_watchdog;
        assert_eq!(fw.check_interval_secs, 30);
        assert_eq!(fw.suspend_gap_secs, 120);
        assert_eq!(fw.stale_after_secs, 90);
        assert_eq!(fw.realert_interval_secs, 1800);
        assert_eq!(fw.notify_retry_attempts, 3);
        assert_eq!(fw.notify_retry_initial_secs, 10);
    }

    fn valid() -> FeedWatchdogConfig {
        FeedWatchdogConfig {
            enabled: true,
            check_interval_secs: 60,
            suspend_gap_secs: 300,
            stale_after_secs: 600,
            realert_interval_secs: 3600,
            exclude_exchanges: Vec::new(),
            notify_retry_attempts: 5,
            notify_retry_initial_secs: 30,
        }
    }

    #[test]
    fn validate_accepts_defaults() {
        valid().validate().unwrap();
    }

    #[test]
    fn validate_rejects_zero_value_for_each_positive_field() {
        type ZeroSetter = fn(&mut FeedWatchdogConfig);
        let cases: [(&str, ZeroSetter); 5] = [
            ("check_interval_secs", |c| c.check_interval_secs = 0),
            ("suspend_gap_secs", |c| c.suspend_gap_secs = 0),
            ("stale_after_secs", |c| c.stale_after_secs = 0),
            ("realert_interval_secs", |c| c.realert_interval_secs = 0),
            ("notify_retry_initial_secs", |c| {
                c.notify_retry_initial_secs = 0
            }),
        ];
        for (field, set_zero) in cases {
            let mut c = valid();
            set_zero(&mut c);
            let err = c.validate().unwrap_err().to_string();
            assert!(err.contains(field), "field={field} err={err}");
        }
    }

    #[test]
    fn validate_accepts_zero_notify_retry_attempts() {
        let mut c = valid();
        c.notify_retry_attempts = 0;
        c.validate().unwrap();
    }

    #[test]
    fn validate_rejects_suspend_gap_equal_to_check_interval() {
        let mut c = valid();
        c.suspend_gap_secs = c.check_interval_secs;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("suspend_gap_secs"));
    }

    #[test]
    fn validate_rejects_suspend_gap_less_than_check_interval() {
        let mut c = valid();
        c.suspend_gap_secs = c.check_interval_secs - 1;
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_accepts_stale_after_equal_to_check_interval() {
        let mut c = valid();
        c.stale_after_secs = c.check_interval_secs;
        c.validate().unwrap();
    }

    #[test]
    fn validate_rejects_stale_after_less_than_check_interval() {
        let mut c = valid();
        c.stale_after_secs = c.check_interval_secs - 1;
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_unknown_exclude_exchange() {
        let mut c = valid();
        c.exclude_exchanges = vec!["unknown_exchange".to_string()];
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("unknown_exchange"), "err={err}");
    }

    #[test]
    fn validate_accepts_known_exclude_exchange() {
        let mut c = valid();
        c.exclude_exchanges = vec!["oanda".to_string()];
        c.validate().unwrap();
    }

    #[test]
    fn validate_accepts_empty_exclude_exchanges() {
        let mut c = valid();
        c.exclude_exchanges = Vec::new();
        c.validate().unwrap();
    }

    #[test]
    fn validate_accepts_notify_retry_attempts_at_cap_of_ten() {
        let mut c = valid();
        c.notify_retry_attempts = 10;
        c.notify_retry_initial_secs = 1;
        c.realert_interval_secs = 1134;
        c.validate().unwrap();
    }

    #[test]
    fn validate_rejects_cap_of_ten_attempts_when_worst_case_equals_realert_interval() {
        let mut c = valid();
        c.notify_retry_attempts = 10;
        c.notify_retry_initial_secs = 1;
        c.realert_interval_secs = 1133;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("worst-case notify time"), "err={err}");
    }

    #[test]
    fn validate_rejects_zero_retry_attempts_when_single_send_timeout_equals_realert_interval() {
        let mut c = valid();
        c.notify_retry_attempts = 0;
        c.realert_interval_secs = 10;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("worst-case notify time"), "err={err}");
    }

    #[test]
    fn validate_accepts_zero_retry_attempts_when_realert_interval_exceeds_single_send_timeout() {
        let mut c = valid();
        c.notify_retry_attempts = 0;
        c.realert_interval_secs = 11;
        c.validate().unwrap();
    }

    #[test]
    fn validate_rejects_notify_retry_attempts_above_cap_without_overflow() {
        let cases: [(u64, u64, u64); 2] = [(11, 1, u64::MAX), (u64::MAX, u64::MAX, u64::MAX)];
        for (attempts, initial_secs, realert_secs) in cases {
            let mut c = valid();
            c.notify_retry_attempts = attempts;
            c.notify_retry_initial_secs = initial_secs;
            c.realert_interval_secs = realert_secs;
            let err = c.validate().unwrap_err().to_string();
            assert!(
                err.contains("notify_retry_attempts"),
                "attempts={attempts} err={err}"
            );
        }
    }

    #[test]
    fn validate_rejects_max_initial_secs_at_cap_without_overflow() {
        let mut c = valid();
        c.notify_retry_attempts = 10;
        c.notify_retry_initial_secs = u64::MAX;
        c.realert_interval_secs = u64::MAX;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("worst-case notify time"), "err={err}");
    }

    #[test]
    fn validate_rejects_worst_case_equal_to_realert_interval() {
        let mut c = valid();
        c.notify_retry_attempts = 5;
        c.notify_retry_initial_secs = 30;
        c.realert_interval_secs = 990;
        let err = c.validate().unwrap_err().to_string();
        assert!(
            err.contains("worst-case notify time"),
            "worst case equal to realert_interval_secs must be rejected: err={err}"
        );
    }

    #[test]
    fn validate_rejects_when_send_timeouts_push_worst_case_past_realert_interval() {
        let mut c = valid();
        c.notify_retry_attempts = 5;
        c.notify_retry_initial_secs = 30;
        c.realert_interval_secs = 950;
        let err = c.validate().unwrap_err().to_string();
        assert!(
            err.contains("realert_interval_secs"),
            "worst case (incl. send timeout) must be rejected: err={err}"
        );
    }

    #[test]
    fn validate_accepts_worst_case_one_second_below_realert_interval() {
        let mut c = valid();
        c.notify_retry_attempts = 5;
        c.notify_retry_initial_secs = 30;
        c.realert_interval_secs = 991;
        c.validate().unwrap();
    }

    #[test]
    fn validate_accepts_issue_109_default_notify_retry_settings() {
        FeedWatchdogConfig::default().validate().unwrap();
    }

    #[test]
    fn repo_default_toml_is_valid() {
        let content = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/default.toml"),
        )
        .expect("config/default.toml must exist");
        let config: AppConfig = toml::from_str(&content).expect("default.toml must parse");
        config
            .validate()
            .expect("default.toml must pass validation");
    }
}

#[test]
fn load_reports_config_path_when_file_is_missing() {
    let path = Path::new("/nonexistent-dir-for-auto-trader-test/missing-config.toml");
    let err = AppConfig::load(path).unwrap_err().to_string();
    assert!(
        err.starts_with(
            "failed to read config file /nonexistent-dir-for-auto-trader-test/missing-config.toml: "
        ),
        "err={err}"
    );
}
