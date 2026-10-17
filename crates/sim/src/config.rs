use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::SimError;

/// Simulator settings read from the `[sim]` table of the configuration file.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct SimConfig {
    /// Base URL of the GMO public API that backfill fetches klines from.
    pub gmo_public_base_url: String,
    /// Number of M5 bars before the evaluation period that are kept for indicator warmup.
    pub warmup_bars: usize,
    /// Distance of the protective stop from the entry price, in pips.
    pub protective_stop_pips: i64,
    /// Swing sizes in pips, one benchmark per value.
    pub thetas_pips: Vec<i64>,
    /// Default number of parallel workers of a sweep.
    pub jobs: usize,
    /// Maximum number of script operations in one bar.
    pub max_operations_per_bar: u64,
    /// Maximum number of script operations in one run.
    pub max_operations_per_run: u64,
    /// Size limit of the indicator cache in megabytes.
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
    /// Checks that every setting is in its valid range.
    ///
    /// # Errors
    ///
    /// - Returns an error if `warmup_bars`, `protective_stop_pips`, `jobs`, `max_operations_per_bar`, `max_operations_per_run` or `indicator_cache_mb` is less than 1.
    /// - Returns an error if `thetas_pips` is empty, contains a value less than 1, or contains a duplicate.
    pub fn validate(&self) -> Result<(), SimError> {
        if self.warmup_bars < 1 {
            return Err(SimError::Config(
                "invalid option \"sim.warmup_bars\": must be >= 1".to_string(),
            ));
        }
        if self.protective_stop_pips < 1 {
            return Err(SimError::Config(
                "invalid option \"sim.protective_stop_pips\": must be >= 1".to_string(),
            ));
        }
        if self.thetas_pips.is_empty() {
            return Err(SimError::Config(
                "invalid option \"sim.thetas_pips\": must not be empty".to_string(),
            ));
        }
        if self.thetas_pips.iter().any(|&v| v < 1) {
            return Err(SimError::Config(
                "invalid option \"sim.thetas_pips\": values must be >= 1".to_string(),
            ));
        }
        {
            let mut sorted = self.thetas_pips.clone();
            sorted.sort_unstable();
            let has_duplicate = sorted.windows(2).any(|w| w[0] == w[1]);
            if has_duplicate {
                return Err(SimError::Config(
                    "invalid option \"sim.thetas_pips\": must not contain duplicates".to_string(),
                ));
            }
        }
        if self.jobs < 1 {
            return Err(SimError::Config(
                "invalid option \"sim.jobs\": must be >= 1".to_string(),
            ));
        }
        if self.max_operations_per_bar < 1 {
            return Err(SimError::Config(
                "invalid option \"sim.max_operations_per_bar\": must be >= 1".to_string(),
            ));
        }
        if self.max_operations_per_run < 1 {
            return Err(SimError::Config(
                "invalid option \"sim.max_operations_per_run\": must be >= 1".to_string(),
            ));
        }
        if self.indicator_cache_mb < 1 {
            return Err(SimError::Config(
                "invalid option \"sim.indicator_cache_mb\": must be >= 1".to_string(),
            ));
        }
        Ok(())
    }
}

/// Settings of the simulator loaded from the configuration file.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// URL of the database.
    pub database_url: String,
    /// Simulator settings, validated.
    pub sim: SimConfig,
}

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

/// Reads the TOML configuration file at `path` and validates its `[sim]` table.
///
/// # Errors
///
/// - Returns an error if the file cannot be read.
/// - Returns an error if the file is not valid TOML or lacks `[database].url`.
/// - Returns an error if the `[sim]` table fails `SimConfig::validate`.
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

/// Returns the path in the `CONFIG_PATH` environment variable, or `config/default.toml` when it is unset.
pub fn config_path() -> PathBuf {
    resolve_config_path_without_reading_env(std::env::var_os("CONFIG_PATH"))
}

fn resolve_config_path_without_reading_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config/default.toml"))
}

#[cfg(test)]
mod tests;
