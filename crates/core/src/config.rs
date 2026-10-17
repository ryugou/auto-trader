use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use rust_decimal::Decimal;
use serde::Deserialize;

/// Application configuration read from a TOML file.
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    /// OANDA settings, if configured.
    #[serde(default)]
    pub oanda: Option<OandaConfig>,
    /// bitFlyer settings, if configured.
    #[serde(default)]
    pub bitflyer: Option<BitflyerConfig>,
    /// Vegapunk connection settings.
    pub vegapunk: VegapunkConfig,
    /// Database connection settings.
    pub database: DatabaseConfig,
    /// Price monitor settings.
    pub monitor: MonitorConfig,
    /// Pair lists.
    pub pairs: PairsConfig,
    /// Per-pair settings keyed by pair name.
    #[serde(default)]
    pub pair_config: HashMap<String, PairConfig>,
    /// Position sizing settings, if configured; ignored, with a log message at startup.
    #[serde(default)]
    pub position_sizing: Option<PositionSizingConfig>,
    /// Configured strategies.
    #[serde(default)]
    pub strategies: Vec<StrategyConfig>,
    /// Macro analyst settings, if configured.
    #[serde(default)]
    pub macro_analyst: Option<MacroAnalystConfig>,
    /// Gemini settings, if configured.
    #[serde(default)]
    pub gemini: Option<GeminiConfig>,
    /// Live trading settings, if configured.
    #[serde(default)]
    pub live: Option<LiveConfig>,
    /// Risk gate settings, if configured.
    #[serde(default)]
    pub risk: Option<RiskConfig>,
    /// Margin settings keyed by exchange name.
    #[serde(default)]
    pub exchange_margin: HashMap<String, ExchangeMarginConfig>,
    /// GMO FX settings.
    #[serde(default)]
    pub gmo_fx: GmoFxConfig,
    /// Feed watchdog settings.
    #[serde(default)]
    pub feed_watchdog: FeedWatchdogConfig,
}

const MAX_NOTIFY_RETRY_ATTEMPTS: u64 = 10;

/// Time limit of one notification send, in seconds.
pub const NOTIFY_SEND_TIMEOUT_SECS: u64 = 10;

const STALE_AFTER_SECS_SPANNING_BITFLYER_DAILY_MAINTENANCE: u64 = 900;

const EXCHANGES_RECORDING_CANDLE_START_AS_TICK_TIME: &[&str] = &["oanda"];

/// Settings of the feed watchdog.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FeedWatchdogConfig {
    /// Whether the watchdog runs.
    pub enabled: bool,
    /// Interval between checks, in seconds.
    pub check_interval_secs: u64,
    /// Wall-clock gap between two checks, in seconds, from which a suspend is assumed.
    pub suspend_gap_secs: u64,
    /// Age of the latest price, in seconds, from which a feed counts as stale.
    pub stale_after_secs: u64,
    /// Minimum interval between repeated alerts, in seconds.
    pub realert_interval_secs: u64,
    /// Names of the exchanges that are excluded from the watch.
    pub exclude_exchanges: Vec<String>,
    /// Number of retries of a failed notification.
    pub notify_retry_attempts: u64,
    /// Wait before the first retry of a failed notification, in seconds; each later wait doubles.
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
    /// Checks the intervals, the excluded exchanges and the retry settings.
    ///
    /// # Errors
    ///
    /// - Returns an error if `check_interval_secs`, `suspend_gap_secs`, `stale_after_secs`, `realert_interval_secs` or `notify_retry_initial_secs` is zero.
    /// - Returns an error if `suspend_gap_secs` is not greater than `check_interval_secs`.
    /// - Returns an error if `stale_after_secs` is less than `check_interval_secs`.
    /// - Returns an error if an entry of `exclude_exchanges` is not a supported exchange name.
    /// - Returns an error if `notify_retry_attempts` is greater than 10.
    /// - Returns an error if `realert_interval_secs` is not greater than the worst-case notification time: `notify_retry_attempts + 1` sends of `NOTIFY_SEND_TIMEOUT_SECS` each plus the retry waits, which double from `notify_retry_initial_secs`.
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

/// GMO FX settings.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct GmoFxConfig {
    /// Swap settings.
    pub swap: GmoFxSwapConfig,
}

/// GMO FX swap settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GmoFxSwapConfig {
    /// Swap rates keyed by pair name.
    pub rates: HashMap<String, SwapRateEntry>,
    /// Date the rates were updated on, as `YYYY-MM-DD`, if set.
    pub updated_on: Option<String>,
    /// Number of days after which the rates count as stale.
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
    /// Returns `updated_on` parsed as a date, or `None` when it is not set.
    ///
    /// # Errors
    ///
    /// - Returns an error if `updated_on` is set and is not a date in the `%Y-%m-%d` format.
    pub fn parsed_updated_on(&self) -> anyhow::Result<Option<chrono::NaiveDate>> {
        match &self.updated_on {
            None => Ok(None),
            Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(Some)
                .context("invalid option \"gmo_fx.swap.updated_on\": must be YYYY-MM-DD"),
        }
    }

    /// Checks the update date and the maximum age of the rates.
    ///
    /// # Errors
    ///
    /// - Returns an error if `updated_on` is set and is not a date in the `%Y-%m-%d` format.
    /// - Returns an error if `rates` is not empty and `updated_on` is not set.
    /// - Returns an error if `max_age_days` is zero.
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

/// Swap rate of one pair.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct SwapRateEntry {
    /// Daily swap rate of a long position.
    pub long: Decimal,
    /// Daily swap rate of a short position.
    pub short: Decimal,
}

/// OANDA settings.
#[derive(Debug, Deserialize, Clone)]
pub struct OandaConfig {
    /// Base URL of the OANDA API.
    pub api_url: String,
    /// Identifier of the OANDA account.
    pub account_id: String,
}

/// Vegapunk connection settings.
#[derive(Debug, Deserialize, Clone)]
pub struct VegapunkConfig {
    /// Endpoint of the Vegapunk service.
    pub endpoint: String,
    /// Schema name used on the Vegapunk service.
    pub schema: String,
}

/// Database connection settings.
#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    /// Connection URL of the database.
    pub url: String,
}

/// Price monitor settings.
#[derive(Debug, Deserialize, Clone)]
pub struct MonitorConfig {
    /// Interval of the monitor, in seconds.
    pub interval_secs: u64,
    /// Number of days to backfill, if set.
    #[serde(default)]
    pub backfill_days: Option<u64>,
}

/// Pair lists.
#[derive(Debug, Deserialize, Clone)]
pub struct PairsConfig {
    /// FX pairs.
    #[serde(default)]
    pub fx: Vec<String>,
    /// Crypto pairs, if configured.
    #[serde(default)]
    pub crypto: Option<Vec<String>>,
    /// Active pairs.
    #[serde(default)]
    pub active: Vec<String>,
}

/// Settings of one strategy.
#[derive(Debug, Deserialize, Clone)]
pub struct StrategyConfig {
    /// Name of the strategy.
    pub name: String,
    /// Whether the strategy is enabled.
    pub enabled: bool,
    /// Mode the strategy runs in.
    pub mode: String,
    /// Pairs the strategy trades.
    pub pairs: Vec<String>,
    /// Strategy parameters keyed by name.
    #[serde(default)]
    pub params: HashMap<String, toml::Value>,
}

/// Macro analyst settings.
#[derive(Debug, Deserialize, Clone)]
pub struct MacroAnalystConfig {
    /// Whether the macro analyst runs.
    pub enabled: bool,
    /// Interval of the calendar fetch, in seconds; no component reads it.
    pub calendar_interval_secs: u64,
    /// Interval of the news fetch, in seconds.
    pub news_interval_secs: u64,
    /// News sources.
    pub news_sources: Vec<String>,
}

/// Gemini settings.
#[derive(Debug, Deserialize, Clone)]
pub struct GeminiConfig {
    /// Name of the Gemini model.
    pub model: String,
    /// Base URL of the Gemini API.
    pub api_url: String,
}

/// bitFlyer settings.
#[derive(Deserialize, Clone)]
pub struct BitflyerConfig {
    /// URL of the bitFlyer WebSocket feed.
    pub ws_url: String,
    /// Base URL of the bitFlyer API.
    pub api_url: String,
    /// API key; never read from the configuration file.
    #[serde(skip, default)]
    pub api_key: Option<String>,
    /// API secret; never read from the configuration file.
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

/// Settings of one pair.
#[derive(Debug, Deserialize, Clone)]
pub struct PairConfig {
    /// Price unit of the pair.
    pub price_unit: Decimal,
    /// Minimum order size of the pair.
    pub min_order_size: Decimal,
}

/// Margin settings of one exchange.
#[derive(Debug, Deserialize, Clone)]
pub struct ExchangeMarginConfig {
    /// Margin level at which positions are liquidated.
    pub liquidation_margin_level: Decimal,
}

/// Position sizing settings, which are ignored: sizing uses the allocation percentage of each signal.
#[derive(Debug, Deserialize, Clone)]
pub struct PositionSizingConfig {
    /// Name of the ignored sizing method.
    pub method: String,
    /// Risk rate of the ignored sizing method.
    pub risk_rate: Decimal,
}

/// Risk gate settings.
#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    /// Maximum age of the latest price tick, in seconds.
    pub price_freshness_secs: u64,
    /// Daily loss ratio of the day's starting balance at which trading stops; 0.05 when absent.
    #[serde(default = "default_daily_loss_limit_pct")]
    pub daily_loss_limit_pct: Decimal,
    /// Hours that trading stays halted; 24 when absent.
    #[serde(default = "default_halt_hours")]
    pub halt_hours: u64,
    /// Margin buffer used in position sizing; 0.10 when absent.
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
    /// Checks the freshness, the daily loss limit, the halt hours and the margin buffer.
    ///
    /// # Errors
    ///
    /// - Returns an error if `price_freshness_secs` is zero.
    /// - Returns an error if `daily_loss_limit_pct` is not in the open interval from 0 to 1.
    /// - Returns an error if `halt_hours` is zero.
    /// - Returns an error if `sizing_margin_buffer` is negative.
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

/// Live trading settings.
#[derive(Debug, Deserialize, Clone)]
pub struct LiveConfig {
    /// Whether live trading is enabled.
    pub enabled: bool,
    /// Whether live trading runs in dry-run mode.
    pub dry_run: bool,
}

impl LiveConfig {
    /// Accepts every value of the live trading settings.
    ///
    /// # Errors
    ///
    /// - Never returns an error.
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
    /// Reads the TOML file at `path`, parses it as an application configuration and validates it.
    ///
    /// # Errors
    ///
    /// - Returns an error if the file cannot be read.
    /// - Returns an error if the content cannot be parsed as an application configuration, for example when a required field is missing.
    /// - Returns an error if `validate` rejects the parsed configuration.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    /// Checks the risk, pair, swap and feed watchdog settings.
    ///
    /// # Errors
    ///
    /// - Returns an error if `risk` is set and its validation fails.
    /// - Returns an error if a pair in `pairs.fx`, `pairs.crypto`, `pairs.active` or the `pairs` of a strategy does not end with `_JPY`.
    /// - Returns an error if the validation of `gmo_fx.swap` fails.
    /// - Returns an error if the validation of `feed_watchdog` fails.
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
