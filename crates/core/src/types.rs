use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pair(pub String);

impl Pair {
    pub fn new(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exchange {
    Oanda,
    BitflyerCfd,
    GmoFx,
}

impl Exchange {
    pub fn as_str(&self) -> &'static str {
        match self {
            Exchange::Oanda => "oanda",
            Exchange::BitflyerCfd => "bitflyer_cfd",
            Exchange::GmoFx => "gmo_fx",
        }
    }
}

impl std::fmt::Display for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Exchange {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "oanda" => Ok(Exchange::Oanda),
            "bitflyer_cfd" => Ok(Exchange::BitflyerCfd),
            "gmo_fx" => Ok(Exchange::GmoFx),
            other => anyhow::bail!("unsupported exchange: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Long,
    Short,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Long => "long",
            Direction::Short => "short",
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Direction {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "long" => Ok(Direction::Long),
            "short" => Ok(Direction::Short),
            other => anyhow::bail!("unsupported direction: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeStatus {
    Open,
    Closing,
    Closed,
}

impl TradeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TradeStatus::Open => "open",
            TradeStatus::Closing => "closing",
            TradeStatus::Closed => "closed",
        }
    }
}

impl std::fmt::Display for TradeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TradeStatus {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "open" => Ok(TradeStatus::Open),
            "closing" => Ok(TradeStatus::Closing),
            "closed" => Ok(TradeStatus::Closed),
            other => anyhow::bail!("unsupported trade_status: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    TpHit,
    SlHit,
    Manual,
    SignalReverse,
    StrategyMeanReached,
    StrategyTrailingChannel,
    StrategyTrailingMa,
    StrategyIndicatorReversal,
    StrategyTimeLimit,
    Reconciled,
    Liquidation,
}

impl ExitReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExitReason::TpHit => "tp_hit",
            ExitReason::SlHit => "sl_hit",
            ExitReason::Manual => "manual",
            ExitReason::SignalReverse => "signal_reverse",
            ExitReason::StrategyMeanReached => "strategy_mean_reached",
            ExitReason::StrategyTrailingChannel => "strategy_trailing_channel",
            ExitReason::StrategyTrailingMa => "strategy_trailing_ma",
            ExitReason::StrategyIndicatorReversal => "strategy_indicator_reversal",
            ExitReason::StrategyTimeLimit => "strategy_time_limit",
            ExitReason::Reconciled => "reconciled",
            ExitReason::Liquidation => "liquidation",
        }
    }
}

impl std::str::FromStr for ExitReason {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "tp_hit" => Ok(ExitReason::TpHit),
            "sl_hit" => Ok(ExitReason::SlHit),
            "manual" => Ok(ExitReason::Manual),
            "signal_reverse" => Ok(ExitReason::SignalReverse),
            "strategy_mean_reached" => Ok(ExitReason::StrategyMeanReached),
            "strategy_trailing_channel" => Ok(ExitReason::StrategyTrailingChannel),
            "strategy_trailing_ma" => Ok(ExitReason::StrategyTrailingMa),
            "strategy_indicator_reversal" => Ok(ExitReason::StrategyIndicatorReversal),
            "strategy_time_limit" => Ok(ExitReason::StrategyTimeLimit),
            "reconciled" => Ok(ExitReason::Reconciled),
            "liquidation" => Ok(ExitReason::Liquidation),
            other => anyhow::bail!("unsupported exit_reason: {other}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    pub stop_loss_pct: Decimal,
    #[serde(default)]
    pub take_profit_pct: Option<Decimal>,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
    #[serde(default = "default_allocation_pct")]
    pub allocation_pct: Decimal,
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
}

const CONSERVATIVE_HALF_ALLOCATION_PCT: Decimal = Decimal::from_parts(5, 0, 0, false, 1);

fn default_allocation_pct() -> Decimal {
    CONSERVATIVE_HALF_ALLOCATION_PCT
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: Uuid,
    pub account_id: Uuid,
    pub strategy_name: String,
    pub pair: Pair,
    pub exchange: Exchange,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub stop_loss: Decimal,
    pub take_profit: Option<Decimal>,
    pub quantity: Decimal,
    pub leverage: Decimal,
    pub fees: Decimal,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<ExitReason>,
    pub status: TradeStatus,
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub exchange_position_id: Option<String>,
    #[serde(default)]
    pub stop_order_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub trade: Trade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    pub pair: Pair,
    pub exchange: Exchange,
    pub timeframe: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Option<u64>,
    #[serde(default)]
    pub best_bid: Option<Decimal>,
    #[serde(default)]
    pub best_ask: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod tests;
