//! Phase 1: Config loading tests.

use auto_trader_core::config::AppConfig;
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

#[test]
fn config_valid_loads_successfully() {
    let config =
        AppConfig::load(&fixture_path("config_valid.toml")).expect("valid config should load");
    assert_eq!(config.pairs.fx, vec!["USD_JPY"]);
    assert_eq!(
        config.pairs.crypto.as_ref().unwrap(),
        &vec!["FX_BTC_JPY".to_string()]
    );
    assert_eq!(config.strategies.len(), 2);
    assert_eq!(config.strategies[0].name, "bb_mean_revert_v1");
    assert!(config.strategies[0].enabled);
    assert_eq!(config.risk.as_ref().unwrap().price_freshness_secs, 60);
    let live = config.live.as_ref().unwrap();
    assert!(!live.enabled);
    assert!(live.dry_run);
}

#[test]
fn config_missing_vegapunk_fails() {
    let result = AppConfig::load(&fixture_path("config_missing_vegapunk.toml"));
    assert!(
        result.is_err(),
        "config without vegapunk section should fail to parse"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("vegapunk"),
        "error should mention missing vegapunk: {err_msg}"
    );
}

#[test]
fn config_empty_pairs_loads_with_empty_vecs() {
    let config = AppConfig::load(&fixture_path("config_missing_pairs.toml"))
        .expect("empty pairs is valid (strategies may not reference any)");
    assert!(config.pairs.fx.is_empty());
    assert!(
        config.pairs.crypto.as_ref().is_none_or(|v| v.is_empty()),
        "crypto pairs should be empty"
    );
}

#[test]
fn config_invalid_strategy_name_parses_but_register_skips() {
    // The config file itself is valid TOML and deserializes fine.
    // The unknown strategy name only matters at register_strategies() time.
    let config = AppConfig::load(&fixture_path("config_invalid_strategy.toml"))
        .expect("config with unknown strategy name is still valid TOML");
    assert_eq!(config.strategies.len(), 1);
    assert_eq!(config.strategies[0].name, "nonexistent_strategy_xyz");
    assert!(config.strategies[0].enabled);
}

#[test]
fn config_disabled_strategies_parse() {
    let config = AppConfig::load(&fixture_path("config_disabled_strategy.toml"))
        .expect("disabled strategies should parse fine");
    assert_eq!(config.strategies.len(), 2);
    assert!(
        config.strategies.iter().all(|s| !s.enabled),
        "all strategies should be disabled"
    );
}

#[test]
fn config_risk_zero_freshness_fails_validation() {
    // Inline TOML with price_freshness_secs = 0.
    let toml_str = r#"
[vegapunk]
endpoint = "http://localhost:3000"
schema = "test"

[database]
url = "postgresql://test:test@localhost:5432/test"

[monitor]
interval_secs = 60

[pairs]
fx = ["USD_JPY"]

[risk]
price_freshness_secs = 0
"#;
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("config_risk_zero.toml");
    std::fs::write(&temp_file, toml_str).unwrap();
    let result = AppConfig::load(&temp_file);
    assert!(result.is_err(), "zero freshness should fail validation");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("price_freshness_secs"),
        "error should mention price_freshness_secs: {err_msg}"
    );
    std::fs::remove_file(temp_file).ok();
}
