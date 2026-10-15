use super::*;

fn config_with_bitflyer(bitflyer: Option<serde_json::Value>) -> AppConfig {
    let mut config_json = serde_json::json!({
        "vegapunk": {"endpoint": "http://x", "schema": "y"},
        "database": {"url": "postgresql://x"},
        "monitor": {"interval_secs": 60},
        "pairs": {},
    });
    if let Some(bf) = bitflyer {
        config_json["bitflyer"] = bf;
    }
    serde_json::from_value(config_json).expect("config_with_bitflyer must parse")
}

#[test]
fn resolve_config_path_falls_back_to_default_only_when_env_is_unset() {
    let cases = [
        (
            "env_unset_falls_back_to_default",
            None,
            "config/default.toml",
        ),
        (
            "env_set_overrides_default",
            Some("x.toml".to_string()),
            "x.toml",
        ),
        (
            "env_set_to_empty_string_is_kept_as_is",
            Some(String::new()),
            "",
        ),
    ];

    for (name, env_value, expected) in cases {
        let actual = resolve_config_path(env_value);
        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn non_empty_keeps_whitespace_only_values_and_drops_empty_strings() {
    let cases = [
        (
            "non_empty_string_is_kept",
            Some("k".to_string()),
            Some("k".to_string()),
        ),
        ("empty_string_becomes_none", Some(String::new()), None),
        (
            "whitespace_only_is_kept_untrimmed",
            Some(" ".to_string()),
            Some(" ".to_string()),
        ),
        ("none_stays_none", None, None),
    ];

    for (name, input, expected) in cases {
        let actual = non_empty(input);
        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn override_bitflyer_credentials_sets_fields_when_bitflyer_config_present() {
    let mut config = config_with_bitflyer(Some(
        serde_json::json!({"ws_url": "wss://x", "api_url": "https://x"}),
    ));

    override_bitflyer_credentials(
        &mut config,
        Some("key".to_string()),
        Some("secret".to_string()),
    );

    let bf = config.bitflyer.expect("bitflyer config must be present");
    assert_eq!(bf.api_key, Some("key".to_string()));
    assert_eq!(bf.api_secret, Some("secret".to_string()));
}

#[test]
fn override_bitflyer_credentials_is_noop_when_bitflyer_config_absent() {
    let mut config = config_with_bitflyer(None);

    override_bitflyer_credentials(
        &mut config,
        Some("key".to_string()),
        Some("secret".to_string()),
    );

    assert!(config.bitflyer.is_none());
}
