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
    pub macro_analyst: Option<MacroAnalystConfig>,
    #[serde(default)]
    pub gemini: Option<GeminiConfig>,
    #[serde(default)]
    pub live: Option<LiveConfig>,
    #[serde(default)]
    pub risk: Option<RiskConfig>,
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

#[derive(Deserialize, Clone)]
pub struct BitflyerConfig {
    pub ws_url: String,
    pub api_url: String,
    /// BITFLYER_API_KEY env から埋める。config/default.toml には書かない。
    #[serde(skip, default)]
    pub api_key: Option<String>,
    /// BITFLYER_API_SECRET env から埋める。config/default.toml には書かない。
    #[serde(skip, default)]
    pub api_secret: Option<String>,
}

// Debug を derive せず手書きすることで、将来 `tracing::debug!("{:?}", cfg)`
// や panic の unwrap メッセージから api_key / api_secret が漏洩する事故を
// 型レベルで防ぐ。
impl std::fmt::Debug for BitflyerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitflyerConfig")
            .field("ws_url", &self.ws_url)
            .field("api_url", &self.api_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "***redacted***"))
            .field(
                "api_secret",
                &self.api_secret.as_ref().map(|_| "***redacted***"),
            )
            .finish()
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct PairConfig {
    pub price_unit: Decimal,
    pub min_order_size: Decimal,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PositionSizingConfig {
    pub method: String,
    pub risk_rate: Decimal,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    pub daily_loss_limit_pct: Decimal,
    pub price_freshness_secs: u64,
    pub kill_switch_release_jst_hour: u32,
}

impl RiskConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.daily_loss_limit_pct <= Decimal::ZERO || self.daily_loss_limit_pct > Decimal::ONE {
            anyhow::bail!("[risk].daily_loss_limit_pct must be in (0, 1]");
        }
        if self.price_freshness_secs == 0 {
            anyhow::bail!("[risk].price_freshness_secs must be > 0");
        }
        if self.kill_switch_release_jst_hour > 23 {
            anyhow::bail!("[risk].kill_switch_release_jst_hour must be 0..=23");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct LiveConfig {
    /// true の時のみ LiveTrader を起動する。account_type='live' の
    /// アカウントが存在すれば main.rs 起動時に true でなければ fatal。
    pub enabled: bool,
    /// true 中は発注直前で no-op し通知のみ出す (DryRunTrader)。
    /// LIVE_DRY_RUN env が設定されていれば env 優先。
    pub dry_run: bool,
    pub execution_poll_interval_secs: u64,
    pub reconciler_interval_secs: u64,
    pub balance_sync_interval_secs: u64,
}

impl LiveConfig {
    /// 起動時の値域チェック。0 秒 interval は busy loop になるため禁止。
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.execution_poll_interval_secs == 0 {
            anyhow::bail!("[live].execution_poll_interval_secs must be > 0");
        }
        if self.reconciler_interval_secs == 0 {
            anyhow::bail!("[live].reconciler_interval_secs must be > 0");
        }
        if self.balance_sync_interval_secs == 0 {
            anyhow::bail!("[live].balance_sync_interval_secs must be > 0");
        }
        Ok(())
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// AppConfig 全体の妥当性検証。各サブ config の `validate()` を呼ぶ。
    /// 不正値があれば anyhow::Error で起動中断させる。
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(live) = &self.live {
            live.validate()?;
        }
        if let Some(risk) = &self.risk {
            risk.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
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

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn valid_live() -> LiveConfig {
        LiveConfig {
            enabled: false,
            dry_run: true,
            execution_poll_interval_secs: 3,
            reconciler_interval_secs: 300,
            balance_sync_interval_secs: 300,
        }
    }

    #[test]
    fn live_validate_accepts_valid_values() {
        valid_live().validate().unwrap();
    }

    #[test]
    fn live_validate_rejects_zero_intervals() {
        for field in ["execution", "reconciler", "balance"] {
            let mut l = valid_live();
            match field {
                "execution" => l.execution_poll_interval_secs = 0,
                "reconciler" => l.reconciler_interval_secs = 0,
                "balance" => l.balance_sync_interval_secs = 0,
                _ => unreachable!(),
            }
            assert!(l.validate().is_err(), "expected error for {field}=0");
        }
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
execution_poll_interval_secs = 3
reconciler_interval_secs = 300
balance_sync_interval_secs = 300
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
daily_loss_limit_pct = 0.05
price_freshness_secs = 60
kill_switch_release_jst_hour = 0
"#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        let risk = cfg.risk.expect("risk section should parse");
        assert_eq!(risk.price_freshness_secs, 60);
        assert_eq!(risk.kill_switch_release_jst_hour, 0);
        risk.validate().unwrap();
    }

    #[test]
    fn risk_validate_rejects_zero_freshness() {
        use rust_decimal_macros::dec;
        let r = crate::config::RiskConfig {
            daily_loss_limit_pct: dec!(0.05),
            price_freshness_secs: 0,
            kill_switch_release_jst_hour: 0,
        };
        assert!(r.validate().is_err());
    }

    #[test]
    fn risk_validate_rejects_invalid_loss_limit() {
        use rust_decimal_macros::dec;
        let r = crate::config::RiskConfig {
            daily_loss_limit_pct: dec!(0),
            price_freshness_secs: 60,
            kill_switch_release_jst_hour: 0,
        };
        assert!(r.validate().is_err());
    }

    #[test]
    fn bitflyer_config_api_key_starts_as_none() {
        // config ファイル側で書いても #[serde(skip)] で無視される
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
}
