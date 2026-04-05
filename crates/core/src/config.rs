use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub oanda: Option<OandaConfig>,
    #[serde(default)]
    pub bitflyer: Option<BitflyerConfig>,
    pub vegapunk: VegapunkConfig,
    pub database: DatabaseConfig,
    pub monitor: MonitorConfig,
    pub pairs: PairsConfig,
    #[serde(default)]
    pub pair_config: HashMap<String, PairConfig>,
    #[serde(default)]
    pub position_sizing: Option<PositionSizingConfig>,
    #[serde(default)]
    pub strategies: Vec<StrategyConfig>,
    #[serde(default)]
    pub paper_accounts: Vec<PaperAccountConfig>,
    #[serde(default)]
    pub macro_analyst: Option<MacroAnalystConfig>,
    #[serde(default)]
    pub gemini: Option<GeminiConfig>,
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
    /// Number of days to backfill max_drawdown on startup (default: 7).
    #[serde(default)]
    pub backfill_days: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PairsConfig {
    #[serde(default)]
    pub fx: Vec<String>,
    #[serde(default)]
    pub crypto: Option<Vec<String>>,
    // 後方互換: 旧 active フィールドが存在する場合は fx として扱う
    #[serde(default)]
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

#[derive(Debug, Deserialize, Clone)]
pub struct MacroAnalystConfig {
    pub enabled: bool,
    /// Reserved for Phase 1 economic calendar integration. Currently unused.
    pub calendar_interval_secs: u64,
    pub news_interval_secs: u64,
    pub news_sources: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GeminiConfig {
    pub model: String,
    pub api_url: String,
    // api_key is read from GEMINI_API_KEY env var (1Password + direnv)
}

#[derive(Debug, Deserialize, Clone)]
pub struct BitflyerConfig {
    pub ws_url: String,
    pub api_url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PairConfig {
    pub price_unit: Decimal,
    pub min_order_size: Decimal,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PaperAccountConfig {
    pub name: String,
    pub exchange: String,
    pub initial_balance: Decimal,
    pub leverage: Decimal,
    pub strategy: String,
    #[serde(default = "default_currency")]
    pub currency: String,
}

fn default_currency() -> String {
    "JPY".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct PositionSizingConfig {
    pub method: String,
    pub risk_rate: Decimal,
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
        assert_eq!(config.oanda.as_ref().unwrap().api_url, "https://api-fxpractice.oanda.com");
        assert_eq!(config.strategies.len(), 1);
        assert_eq!(config.strategies[0].name, "trend_follow_v1");
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
name = "trend_follow_v1"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]

[[strategies]]
name = "crypto_trend_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY"]

[[paper_accounts]]
name = "crypto_real"
exchange = "bitflyer_cfd"
initial_balance = 5233
leverage = 2
strategy = "crypto_trend_v1"
currency = "JPY"
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.bitflyer.as_ref().unwrap().ws_url, "wss://ws.lightstream.bitflyer.com/json-rpc");
        assert_eq!(config.pairs.crypto.as_ref().unwrap().len(), 1);
        assert_eq!(config.pair_config.get("FX_BTC_JPY").unwrap().price_unit.to_string(), "1");
        assert_eq!(config.paper_accounts.len(), 1);
        assert_eq!(config.paper_accounts[0].name, "crypto_real");
        assert_eq!(config.paper_accounts[0].strategy, "crypto_trend_v1");
        assert_eq!(config.paper_accounts[0].leverage.to_string(), "2");
        assert_eq!(config.position_sizing.as_ref().unwrap().risk_rate.to_string(), "0.02");
    }
}
