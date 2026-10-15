use super::*;

fn valid_sim_config() -> SimConfig {
    SimConfig::default()
}

fn write_process_unique_fixture(name: &str, contents: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{}-{name}", std::process::id()));
    std::fs::write(&path, contents).expect("write fixture toml");
    path
}

#[test]
fn default_matches_spec_4() {
    let cfg = SimConfig::default();
    assert_eq!(
        cfg.gmo_public_base_url,
        "https://forex-api.coin.z.com/public"
    );
    assert_eq!(cfg.warmup_bars, 3000);
    assert_eq!(cfg.protective_stop_pips, 100);
    assert_eq!(cfg.thetas_pips, vec![20, 50, 100]);
    assert_eq!(cfg.jobs, 1);
    assert_eq!(cfg.max_operations_per_bar, 200_000);
    assert_eq!(cfg.max_operations_per_run, 1_000_000_000);
    assert_eq!(cfg.indicator_cache_mb, 512);
}

#[test]
fn load_uses_all_defaults_when_sim_section_is_absent() {
    let path = write_process_unique_fixture(
        "sim_config_test_no_sim_section.toml",
        r#"
[database]
url = "postgresql://u:p@localhost/auto_trader"
"#,
    );
    let settings = load(&path).expect("config without [sim] should still load");
    std::fs::remove_file(&path).ok();
    assert_eq!(settings.sim, SimConfig::default());
    assert_eq!(
        settings.database_url,
        "postgresql://u:p@localhost/auto_trader"
    );
}

#[test]
fn load_fills_missing_keys_with_defaults() {
    let path = write_process_unique_fixture(
        "sim_config_test_partial_sim_section.toml",
        r#"
[database]
url = "postgresql://u:p@localhost/auto_trader"

[sim]
warmup_bars = 500
jobs = 4
"#,
    );
    let settings = load(&path).expect("partial [sim] should load with defaults for the rest");
    std::fs::remove_file(&path).ok();
    assert_eq!(settings.sim.warmup_bars, 500);
    assert_eq!(settings.sim.jobs, 4);
    let defaults = SimConfig::default();
    assert_eq!(
        settings.sim.gmo_public_base_url,
        defaults.gmo_public_base_url
    );
    assert_eq!(
        settings.sim.protective_stop_pips,
        defaults.protective_stop_pips
    );
    assert_eq!(settings.sim.thetas_pips, defaults.thetas_pips);
    assert_eq!(
        settings.sim.max_operations_per_bar,
        defaults.max_operations_per_bar
    );
    assert_eq!(
        settings.sim.max_operations_per_run,
        defaults.max_operations_per_run
    );
    assert_eq!(settings.sim.indicator_cache_mb, defaults.indicator_cache_mb);
}

#[test]
fn load_fails_when_database_url_is_missing() {
    let path = write_process_unique_fixture(
        "sim_config_test_missing_database_url.toml",
        r#"
[sim]
jobs = 2
"#,
    );
    let result = load(&path);
    std::fs::remove_file(&path).ok();
    assert!(matches!(result, Err(SimError::Config(_))));
}

#[test]
fn load_reads_repository_default_toml() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/default.toml");
    let settings = load(&path).expect("repository config/default.toml should load");
    assert_eq!(settings.sim, SimConfig::default());
}

#[test]
fn validate_accepts_defaults() {
    valid_sim_config()
        .validate()
        .expect("defaults must be valid");
}

#[test]
fn validate_rejects_warmup_bars_zero() {
    let cfg = SimConfig {
        warmup_bars: 0,
        ..valid_sim_config()
    };
    assert!(matches!(cfg.validate(), Err(SimError::Config(_))));
}

#[test]
fn validate_rejects_protective_stop_pips_zero() {
    let cfg = SimConfig {
        protective_stop_pips: 0,
        ..valid_sim_config()
    };
    assert!(matches!(cfg.validate(), Err(SimError::Config(_))));
}

#[test]
fn validate_rejects_empty_thetas_pips() {
    let cfg = SimConfig {
        thetas_pips: vec![],
        ..valid_sim_config()
    };
    assert!(matches!(cfg.validate(), Err(SimError::Config(_))));
}

#[test]
fn validate_rejects_duplicate_thetas_pips() {
    let cfg = SimConfig {
        thetas_pips: vec![20, 20],
        ..valid_sim_config()
    };
    assert!(matches!(cfg.validate(), Err(SimError::Config(_))));
}

#[test]
fn validate_rejects_theta_pips_below_one() {
    let cfg = SimConfig {
        thetas_pips: vec![0],
        ..valid_sim_config()
    };
    assert!(matches!(cfg.validate(), Err(SimError::Config(_))));
}

#[test]
fn validate_rejects_jobs_zero() {
    let cfg = SimConfig {
        jobs: 0,
        ..valid_sim_config()
    };
    assert!(matches!(cfg.validate(), Err(SimError::Config(_))));
}

#[test]
fn validate_rejects_max_operations_per_bar_zero() {
    let cfg = SimConfig {
        max_operations_per_bar: 0,
        ..valid_sim_config()
    };
    assert!(matches!(cfg.validate(), Err(SimError::Config(_))));
}

#[test]
fn validate_rejects_max_operations_per_run_zero() {
    let cfg = SimConfig {
        max_operations_per_run: 0,
        ..valid_sim_config()
    };
    assert!(matches!(cfg.validate(), Err(SimError::Config(_))));
}

#[test]
fn validate_rejects_indicator_cache_mb_zero() {
    let cfg = SimConfig {
        indicator_cache_mb: 0,
        ..valid_sim_config()
    };
    assert!(matches!(cfg.validate(), Err(SimError::Config(_))));
}

#[test]
fn load_rejects_invalid_sim_section() {
    let path = write_process_unique_fixture(
        "sim_config_test_invalid_jobs.toml",
        r#"
[database]
url = "postgresql://u:p@localhost/auto_trader"

[sim]
jobs = 0
"#,
    );
    let result = load(&path);
    std::fs::remove_file(&path).ok();
    assert!(matches!(result, Err(SimError::Config(_))));
}

#[test]
fn config_path_defaults_to_config_default_toml_when_unset() {
    assert_eq!(
        resolve_config_path_without_reading_env(None),
        PathBuf::from("config/default.toml")
    );
}

#[test]
fn config_path_honors_env_var_when_set() {
    assert_eq!(
        resolve_config_path_without_reading_env(Some(std::ffi::OsString::from(
            "/tmp/custom-sim-config.toml"
        ))),
        PathBuf::from("/tmp/custom-sim-config.toml")
    );
}
