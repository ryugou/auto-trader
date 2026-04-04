use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub oanda: OandaConfig,
    pub vegapunk: VegapunkConfig,
    pub database: DatabaseConfig,
    pub monitor: MonitorConfig,
    pub pairs: PairsConfig,
    #[serde(default)]
    pub strategies: Vec<StrategyConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OandaConfig {
    pub api_url: String,
    pub account_id: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct VegapunkConfig {
    pub endpoint: String,
    pub schema: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MonitorConfig {
    pub interval_secs: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PairsConfig {
    pub active: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StrategyConfig {
    pub name: String,
    pub enabled: bool,
    pub mode: String,
    pub pairs: Vec<String>,
    #[serde(default)]
    pub params: HashMap<String, toml::Value>,
}

impl AppConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
name = "trend_follow_v1"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]
params = { ma_short = 20, ma_long = 50 }
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.oanda.api_url, "https://api-fxpractice.oanda.com");
        assert_eq!(config.strategies.len(), 1);
        assert_eq!(config.strategies[0].name, "trend_follow_v1");
        assert_eq!(config.pairs.active, vec!["USD_JPY"]);
    }
}
