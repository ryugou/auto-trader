use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use rust_decimal::Decimal;
use serde::Deserialize;

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
    #[serde(default)]
    pub exchange_margin: HashMap<String, ExchangeMarginConfig>,
    #[serde(default)]
    pub gmo_fx: GmoFxConfig,
    #[serde(default)]
    pub feed_watchdog: FeedWatchdogConfig,
}

const MAX_NOTIFY_RETRY_ATTEMPTS: u64 = 10;

pub const NOTIFY_SEND_TIMEOUT_SECS: u64 = 10;

const STALE_AFTER_SECS_SPANNING_BITFLYER_DAILY_MAINTENANCE: u64 = 900;

const EXCHANGES_RECORDING_CANDLE_START_AS_TICK_TIME: &[&str] = &["oanda"];

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FeedWatchdogConfig {
    pub enabled: bool,
    pub check_interval_secs: u64,
    pub suspend_gap_secs: u64,
    pub stale_after_secs: u64,
    pub realert_interval_secs: u64,
    pub exclude_exchanges: Vec<String>,
    pub notify_retry_attempts: u64,
    pub notify_retry_initial_secs: u64,
}

impl Default for FeedWatchdogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_secs: 60,
            suspend_gap_secs: 300,
            stale_after_secs: STALE_AFTER_SECS_SPANNING_BITFLYER_DAILY_MAINTENANCE,
            realert_interval_secs: 3600,
            exclude_exchanges: EXCHANGES_RECORDING_CANDLE_START_AS_TICK_TIME
                .iter()
                .map(|s| s.to_string())
                .collect(),
            notify_retry_attempts: 5,
            notify_retry_initial_secs: 30,
        }
    }
}

impl FeedWatchdogConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.check_interval_secs == 0 {
            anyhow::bail!("invalid option \"feed_watchdog.check_interval_secs\": must be >= 1");
        }
        if self.suspend_gap_secs == 0 {
            anyhow::bail!("invalid option \"feed_watchdog.suspend_gap_secs\": must be >= 1");
        }
        if self.stale_after_secs == 0 {
            anyhow::bail!("invalid option \"feed_watchdog.stale_after_secs\": must be >= 1");
        }
        if self.realert_interval_secs == 0 {
            anyhow::bail!("invalid option \"feed_watchdog.realert_interval_secs\": must be >= 1");
        }
        if self.notify_retry_initial_secs == 0 {
            anyhow::bail!(
                "invalid option \"feed_watchdog.notify_retry_initial_secs\": must be >= 1"
            );
        }
        if self.suspend_gap_secs <= self.check_interval_secs {
            anyhow::bail!(
                "invalid option \"feed_watchdog.suspend_gap_secs\": must be > check_interval_secs"
            );
        }
        if self.stale_after_secs < self.check_interval_secs {
            anyhow::bail!(
                "invalid option \"feed_watchdog.stale_after_secs\": must be >= check_interval_secs"
            );
        }
        for ex in &self.exclude_exchanges {
            if let Err(e) = ex.parse::<crate::types::Exchange>() {
                anyhow::bail!("invalid option \"feed_watchdog.exclude_exchanges\": {e}");
            }
        }
        if self.notify_retry_attempts > MAX_NOTIFY_RETRY_ATTEMPTS {
            anyhow::bail!(
                "invalid option \"feed_watchdog.notify_retry_attempts\": must be <= {MAX_NOTIFY_RETRY_ATTEMPTS}"
            );
        }
        let total_wait_secs: u128 = (2u128.pow(self.notify_retry_attempts as u32) - 1)
            * self.notify_retry_initial_secs as u128;
        let send_count: u128 = self.notify_retry_attempts as u128 + 1;
        let worst_case_secs: u128 = send_count * NOTIFY_SEND_TIMEOUT_SECS as u128 + total_wait_secs;
        if worst_case_secs >= self.realert_interval_secs as u128 {
            anyhow::bail!(
                "invalid option \"feed_watchdog.realert_interval_secs\": must be > worst-case notify time \
                 ({worst_case_secs}s = total retry wait {total_wait_secs}s + \
                 {send_count} sends * NOTIFY_SEND_TIMEOUT_SECS {NOTIFY_SEND_TIMEOUT_SECS}s)"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct GmoFxConfig {
    pub swap: GmoFxSwapConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GmoFxSwapConfig {
    pub rates: HashMap<String, SwapRateEntry>,
    pub updated_on: Option<String>,
    #[serde(default = "default_swap_max_age_days")]
    pub max_age_days: u32,
}

const SWAP_MAX_AGE_DAYS_MONTHLY_UPDATE_PLUS_GRACE: u32 = 35;

fn default_swap_max_age_days() -> u32 {
    SWAP_MAX_AGE_DAYS_MONTHLY_UPDATE_PLUS_GRACE
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
    pub fn parsed_updated_on(&self) -> anyhow::Result<Option<chrono::NaiveDate>> {
        match &self.updated_on {
            None => Ok(None),
            Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(Some)
                .context("invalid option \"gmo_fx.swap.updated_on\": must be YYYY-MM-DD"),
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let parsed = self.parsed_updated_on()?;
        if !self.rates.is_empty() && parsed.is_none() {
            anyhow::bail!("invalid option \"gmo_fx.swap.updated_on\": required when rates are set");
        }
        if self.max_age_days == 0 {
            anyhow::bail!("invalid option \"gmo_fx.swap.max_age_days\": must be > 0");
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
    #[serde(default)]
    pub backfill_days: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PairsConfig {
    #[serde(default)]
    pub fx: Vec<String>,
    #[serde(default)]
    pub crypto: Option<Vec<String>>,
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
    pub calendar_interval_secs: u64,
    pub news_interval_secs: u64,
    pub news_sources: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GeminiConfig {
    pub model: String,
    pub api_url: String,
}

#[derive(Deserialize, Clone)]
pub struct BitflyerConfig {
    pub ws_url: String,
    pub api_url: String,
    #[serde(skip, default)]
    pub api_key: Option<String>,
    #[serde(skip, default)]
    pub api_secret: Option<String>,
}

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
    #[serde(default = "default_daily_loss_limit_pct")]
    pub daily_loss_limit_pct: Decimal,
    #[serde(default = "default_halt_hours")]
    pub halt_hours: u64,
    #[serde(default = "default_sizing_margin_buffer")]
    pub sizing_margin_buffer: Decimal,
}

fn default_daily_loss_limit_pct() -> Decimal {
    Decimal::new(5, 2)
}

fn default_halt_hours() -> u64 {
    24
}

fn default_sizing_margin_buffer() -> Decimal {
    Decimal::new(10, 2)
}

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
            anyhow::bail!("invalid option \"risk.price_freshness_secs\": must be > 0");
        }
        if self.daily_loss_limit_pct <= Decimal::ZERO || self.daily_loss_limit_pct >= Decimal::ONE {
            anyhow::bail!("invalid option \"risk.daily_loss_limit_pct\": must be in (0, 1)");
        }
        if self.halt_hours == 0 {
            anyhow::bail!("invalid option \"risk.halt_hours\": must be > 0");
        }
        if self.sizing_margin_buffer < Decimal::ZERO {
            anyhow::bail!("invalid option \"risk.sizing_margin_buffer\": must be >= 0");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct LiveConfig {
    pub enabled: bool,
    pub dry_run: bool,
}

impl LiveConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

fn ensure_jpy_quote(pairs: &[String], section: &str) -> anyhow::Result<()> {
    for p in pairs {
        if !p.ends_with("_JPY") {
            anyhow::bail!("invalid option \"{section}\": pair \"{p}\" must be quoted in JPY");
        }
    }
    Ok(())
}

impl AppConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

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
            ensure_jpy_quote(&s.pairs, &format!("strategies.{}.pairs", s.name))?;
        }
        self.gmo_fx.swap.validate()?;
        self.feed_watchdog.validate()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
