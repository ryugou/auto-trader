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
    /// Per-exchange margin settings (TOML key `[exchange_margin.<exchange>]`).
    /// Each entry's `liquidation_margin_level` is the broker's margin-call
    /// threshold expressed as a decimal (e.g., 0.50 for bitFlyer Crypto CFD,
    /// 1.00 for GMOコイン外国為替FX). PositionSizer caps allocation so the
    /// post-SL margin level does not fall below this threshold.
    #[serde(default)]
    pub exchange_margin: HashMap<String, ExchangeMarginConfig>,
    /// GMO FX swap rates (TOML key `[gmo_fx.swap]`).
    /// 未設定なら空 HashMap (該当 pair の swap 計上は 0)。
    #[serde(default)]
    pub gmo_fx: GmoFxConfig,
}

/// GMO FX 用 config (TOML key `[gmo_fx]`)。現状は swap rate のみ。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct GmoFxConfig {
    pub swap: GmoFxSwapConfig,
}

/// Per-pair × per-direction swap rates for GMO FX (in JPY per lot per day).
///
/// signed: **positive = paper account pays, negative = receives**.
/// `apply_swap_fee` / `compute_daily_swap` の符号規約と一致
/// (`fee_amount` が正 → fees 増・balance 減 = paper 払い)。
///
/// 1 lot = 10_000 通貨単位 (GMO FX 標準)。
///
/// TOML 例:
/// ```toml
/// [gmo_fx.swap.rates]
/// USD_JPY = { long = 100, short = -120 }
/// EUR_JPY = { long = 80, short = -100 }
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GmoFxSwapConfig {
    pub rates: HashMap<String, SwapRateEntry>,
    /// rates を最後に GMO 公表スワップカレンダーと突合した日 ("YYYY-MM-DD")。
    /// rates が非空なら必須 (validate で強制)。
    pub updated_on: Option<String>,
    /// updated_on からこの日数を超えたら staleness アラート。
    #[serde(default = "default_swap_max_age_days")]
    pub max_age_days: u32,
}

fn default_swap_max_age_days() -> u32 {
    35 // 月次更新運用 + 猶予
}

impl Default for GmoFxSwapConfig {
    fn default() -> Self {
        Self {
            rates: HashMap::new(),
            updated_on: None,
            max_age_days: default_swap_max_age_days(),
        }
    }
}

impl GmoFxSwapConfig {
    /// updated_on を NaiveDate として返す。rates 非空なのに未設定/不正なら Err。
    pub fn parsed_updated_on(&self) -> anyhow::Result<Option<chrono::NaiveDate>> {
        match &self.updated_on {
            None => Ok(None),
            Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(Some)
                .map_err(|e| {
                    anyhow::anyhow!("[gmo_fx.swap].updated_on '{s}' is not YYYY-MM-DD: {e}")
                }),
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let parsed = self.parsed_updated_on()?;
        if !self.rates.is_empty() && parsed.is_none() {
            anyhow::bail!(
                "[gmo_fx.swap].updated_on is required when rates are set \
                 (staleness tracking needs a reference date)"
            );
        }
        if self.max_age_days == 0 {
            anyhow::bail!("[gmo_fx.swap].max_age_days must be > 0");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct SwapRateEntry {
    pub long: Decimal,
    pub short: Decimal,
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
pub struct ExchangeMarginConfig {
    pub liquidation_margin_level: Decimal,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PositionSizingConfig {
    pub method: String,
    pub risk_rate: Decimal,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    pub price_freshness_secs: u64,
    /// Kill Switch: 当日実現損失が開始残高のこの割合に達したら新規停止。
    #[serde(default = "default_daily_loss_limit_pct")]
    pub daily_loss_limit_pct: Decimal,
    /// Kill Switch 発火後に新規エントリーを止める時間 (時間単位)。
    #[serde(default = "default_halt_hours")]
    pub halt_hours: u64,
    /// PositionSizer が各取引所のロスカット閾値 Y に上乗せする安全バッファ。
    /// max_alloc = 1 / (Y + buffer + L×s)。0 なら Y ちょうどを狙う。
    #[serde(default = "default_sizing_margin_buffer")]
    pub sizing_margin_buffer: Decimal,
}

fn default_daily_loss_limit_pct() -> Decimal {
    Decimal::new(5, 2) // 0.05
}

fn default_halt_hours() -> u64 {
    24
}

fn default_sizing_margin_buffer() -> Decimal {
    Decimal::new(10, 2) // 0.10
}

/// `[risk]` セクション自体が config に無い時 (`AppConfig::risk == None`) の
/// フォールバック値。各 field の serde default 関数と厳密に一致させること
/// (`price_freshness_secs` のみ serde default が無いため 60 を直接指定 —
/// `main.rs` が長らく使っていたハードコード値と同じ)。
/// `risk_config_default_matches_serde_defaults` テストで serde 側との
/// 一致を保証する。
impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            price_freshness_secs: 60,
            daily_loss_limit_pct: default_daily_loss_limit_pct(),
            halt_hours: default_halt_hours(),
            sizing_margin_buffer: default_sizing_margin_buffer(),
        }
    }
}

impl RiskConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.price_freshness_secs == 0 {
            anyhow::bail!("[risk].price_freshness_secs must be > 0");
        }
        if self.daily_loss_limit_pct <= Decimal::ZERO || self.daily_loss_limit_pct >= Decimal::ONE {
            anyhow::bail!("[risk].daily_loss_limit_pct must be in (0, 1)");
        }
        if self.halt_hours == 0 {
            anyhow::bail!("[risk].halt_hours must be > 0");
        }
        if self.sizing_margin_buffer < Decimal::ZERO {
            anyhow::bail!("[risk].sizing_margin_buffer must be >= 0");
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
}

impl LiveConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// 口座は全て JPY 建て。quote 通貨が JPY でないペア (EUR_USD 等) は、
/// position_sizer / margin が price×qty を JPY 金額として扱う前提と矛盾し
/// 証拠金・維持率を誤算するため、起動時に拒否する。
/// cross-currency 換算を実装するまでこのガードを外してはならない。
fn ensure_jpy_quote(pairs: &[String], section: &str) -> anyhow::Result<()> {
    for p in pairs {
        if !p.ends_with("_JPY") {
            anyhow::bail!(
                "[{section}] pair '{p}' is not JPY-quoted; \
                 non-JPY quote pairs are unsupported (margin math assumes price×qty is JPY)"
            );
        }
    }
    Ok(())
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
        ensure_jpy_quote(&self.pairs.fx, "pairs.fx")?;
        if let Some(crypto) = &self.pairs.crypto {
            ensure_jpy_quote(crypto, "pairs.crypto")?;
        }
        ensure_jpy_quote(&self.pairs.active, "pairs.active")?;
        for s in &self.strategies {
            ensure_jpy_quote(&s.pairs, &format!("strategies({})", s.name))?;
        }
        self.gmo_fx.swap.validate()?;
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

#[cfg(test)]
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
        // price_freshness_secs のみ指定 → daily_loss_limit_pct / halt_hours は
        // serde default (0.05 / 24) で埋まる。
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
        // `RiskConfig::default()` (config.risk == None のフォールバック) は
        // 各 field の serde default 関数と一致していなければならない。
        // price_freshness_secs のみ serde default が無いため、[risk] に
        // 明示指定した TOML との一致で確認する。
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
    fn swap_rates_without_updated_on_fail_validation() {
        // rates があるのに updated_on 無し → 起動拒否 (鮮度管理の起点が無い)
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
        // updated_on = "not-a-date" → 起動拒否
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
        // [gmo_fx.swap] 自体が無い既存 config は従来どおり valid
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
}
