use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::SimError;

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

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub database_url: String,
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
