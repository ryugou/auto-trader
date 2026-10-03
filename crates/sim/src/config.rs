//! `auto-trader-sim` の設定の読み込みと検証（spec 4 章）。
//!
//! `auto-trader-sim` は `AppConfig::load`（`auto-trader-core`）を使わない。
//! 設定ファイルから `[database].url` と `[sim]` だけを読み、他のセクション
//! （`[oanda]` など売買プロセス用の設定）は無視する。

use crate::error::SimError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `[sim]` セクション。キーが欠けている場合は `Default` の値を使う
/// (`#[serde(default)]` がフィールドごとに `SimConfig::default()` から補う)。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct SimConfig {
    pub gmo_public_base_url: String,
    pub warmup_bars: usize,
    pub protective_stop_pips: i64,
    pub thetas_pips: Vec<i64>,
    pub jobs: usize,
    pub max_operations_per_bar: u64,
    pub max_operations_per_run: u64,
    pub indicator_cache_mb: usize,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            gmo_public_base_url: "https://forex-api.coin.z.com/public".to_string(),
            warmup_bars: 3000,
            protective_stop_pips: 100,
            thetas_pips: vec![20, 50, 100],
            jobs: 1,
            max_operations_per_bar: 200_000,
            max_operations_per_run: 1_000_000_000,
            indicator_cache_mb: 512,
        }
    }
}

impl SimConfig {
    /// spec 4 章の検証条件。違反は `SimError::Config` とし、どのキーがなぜ
    /// 不正かをメッセージに含める（運用者が設定ファイルを直接直せるように）。
    pub fn validate(&self) -> Result<(), SimError> {
        if self.warmup_bars < 1 {
            return Err(SimError::Config(format!(
                "sim.warmup_bars must be >= 1, got {}",
                self.warmup_bars
            )));
        }
        if self.protective_stop_pips < 1 {
            return Err(SimError::Config(format!(
                "sim.protective_stop_pips must be >= 1, got {}",
                self.protective_stop_pips
            )));
        }
        if self.thetas_pips.is_empty() {
            return Err(SimError::Config(
                "sim.thetas_pips must not be empty".to_string(),
            ));
        }
        if let Some(&bad) = self.thetas_pips.iter().find(|&&v| v < 1) {
            return Err(SimError::Config(format!(
                "sim.thetas_pips values must be >= 1, got {bad}"
            )));
        }
        {
            let mut sorted = self.thetas_pips.clone();
            sorted.sort_unstable();
            let has_duplicate = sorted.windows(2).any(|w| w[0] == w[1]);
            if has_duplicate {
                return Err(SimError::Config(format!(
                    "sim.thetas_pips must not contain duplicates, got {:?}",
                    self.thetas_pips
                )));
            }
        }
        if self.jobs < 1 {
            return Err(SimError::Config(format!(
                "sim.jobs must be >= 1, got {}",
                self.jobs
            )));
        }
        if self.max_operations_per_bar < 1 {
            return Err(SimError::Config(format!(
                "sim.max_operations_per_bar must be >= 1, got {}",
                self.max_operations_per_bar
            )));
        }
        if self.max_operations_per_run < 1 {
            return Err(SimError::Config(format!(
                "sim.max_operations_per_run must be >= 1, got {}",
                self.max_operations_per_run
            )));
        }
        if self.indicator_cache_mb < 1 {
            return Err(SimError::Config(format!(
                "sim.indicator_cache_mb must be >= 1, got {}",
                self.indicator_cache_mb
            )));
        }
        Ok(())
    }
}

/// `auto-trader-sim` が実際に使う設定一式。`AppConfig` と違い、売買プロセス
/// 用のセクションは保持しない。
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub database_url: String,
    pub sim: SimConfig,
}

/// `[database].url` だけを要求する最小の TOML 表現。他のセクション
/// (`[oanda]` 等) は無視する(serde のデフォルト挙動: 未知フィールドは無視)。
#[derive(Deserialize)]
struct RawDatabase {
    url: String,
}

#[derive(Deserialize)]
struct RawRoot {
    database: RawDatabase,
    #[serde(default)]
    sim: SimConfig,
}

/// `path` から `[database].url` と `[sim]` を読み、検証まで行う。
///
/// `AppConfig::load`（`auto-trader-core`）とは独立した実装である: 本番の売買
/// プロセス用設定とは異なるセクションの集合を読むため、`auto-trader-core` に
/// 依存させない(spec 4 章)。
pub fn load(path: &Path) -> Result<Settings, SimError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        SimError::Config(format!(
            "failed to read config file {}: {e}",
            path.display()
        ))
    })?;
    let raw: RawRoot = toml::from_str(&content).map_err(|e| {
        SimError::Config(format!(
            "failed to parse config file {} (expects [database].url and optionally [sim]): {e}",
            path.display()
        ))
    })?;
    raw.sim.validate()?;
    Ok(Settings {
        database_url: raw.database.url,
        sim: raw.sim,
    })
}

/// 設定ファイルのパス。環境変数 `CONFIG_PATH`、未設定なら `config/default.toml`。
pub fn config_path() -> PathBuf {
    config_path_from(std::env::var_os("CONFIG_PATH"))
}

/// `config_path` の純粋部分。環境変数に触れないため、テストを並列実行できる。
fn config_path_from(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config/default.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_sim_config() -> SimConfig {
        SimConfig::default()
    }

    fn write_fixture(name: &str, contents: &str) -> PathBuf {
        // 別プロセスで同時に走るテストと衝突しないよう、pid を含めて一意にする。
        let path = std::env::temp_dir().join(format!("{}-{name}", std::process::id()));
        std::fs::write(&path, contents).expect("write fixture toml");
        path
    }

    // ---- SimConfig::default / Deserialize -------------------------------

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

    // ---- load(): [sim] セクションの欠落・部分指定 -------------------------

    #[test]
    fn load_uses_all_defaults_when_sim_section_is_absent() {
        let path = write_fixture(
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
        let path = write_fixture(
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
        // 指定しなかったキーは既定値のまま。
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
        let path = write_fixture(
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
        // リポジトリ直下の config/default.toml を実際に読む。
        // crates/sim から見て ../../config/default.toml。
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/default.toml");
        let settings = load(&path).expect("repository config/default.toml should load");
        assert_eq!(settings.sim, SimConfig::default());
    }

    // ---- SimConfig::validate(): spec 4 章の検証条件 1 件ずつ ---------------

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
        // load() は検証まで行う (spec: `[database].url` と `[sim]` を読み、validate まで行う)。
        let path = write_fixture(
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

    // ---- config_path() ----------------------------------------------------

    // 環境変数は変更しない(並列実行下の競合と、マルチスレッド下の set_var を避ける)。
    #[test]
    fn config_path_defaults_to_config_default_toml_when_unset() {
        assert_eq!(config_path_from(None), PathBuf::from("config/default.toml"));
    }

    #[test]
    fn config_path_honors_env_var_when_set() {
        assert_eq!(
            config_path_from(Some(std::ffi::OsString::from(
                "/tmp/custom-sim-config.toml"
            ))),
            PathBuf::from("/tmp/custom-sim-config.toml")
        );
    }
}
