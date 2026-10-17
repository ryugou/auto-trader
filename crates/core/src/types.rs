use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Currency pair identified by its name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pair(
    /// Name of the pair.
    pub String,
);

impl Pair {
    /// Creates a pair from its name.
    pub fn new(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Exchange that prices come from and orders go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exchange {
    /// OANDA.
    Oanda,
    /// bitFlyer CFD.
    BitflyerCfd,
    /// GMO FX.
    GmoFx,
}

impl Exchange {
    /// Returns the snake_case name of the exchange.
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

/// Direction of a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Buying position.
    Long,
    /// Selling position.
    Short,
}

impl Direction {
    /// Returns the snake_case name of the direction.
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

/// Lifecycle status of a trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeStatus {
    /// The trade is open.
    Open,
    /// The trade is being closed.
    Closing,
    /// The trade is closed.
    Closed,
}

impl TradeStatus {
    /// Returns the snake_case name of the status.
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

/// Reason a trade was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// The take profit was hit.
    TpHit,
    /// The stop loss was hit.
    SlHit,
    /// The trade was closed manually.
    Manual,
    /// A signal in the opposite direction closed the trade.
    SignalReverse,
    /// The strategy closed the trade because the price reached the mean.
    StrategyMeanReached,
    /// The strategy closed the trade because the price hit the trailing channel.
    StrategyTrailingChannel,
    /// The strategy closed the trade because the price hit the trailing moving average.
    StrategyTrailingMa,
    /// The strategy closed the trade because an indicator reversed.
    StrategyIndicatorReversal,
    /// The strategy closed the trade because the holding time limit was reached.
    StrategyTimeLimit,
    /// The trade was closed by reconciliation with the exchange.
    Reconciled,
    /// The position was liquidated.
    Liquidation,
}

impl ExitReason {
    /// Returns the snake_case name of the reason.
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

/// Trading signal produced by a strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    /// Name of the strategy that produced the signal.
    pub strategy_name: String,
    /// Pair to trade.
    pub pair: Pair,
    /// Direction to trade in.
    pub direction: Direction,
    /// Stop loss distance as a ratio of the entry price.
    pub stop_loss_pct: Decimal,
    /// Take profit distance as a ratio of the entry price, if any.
    #[serde(default)]
    pub take_profit_pct: Option<Decimal>,
    /// Confidence of the strategy in the signal.
    pub confidence: f64,
    /// Time the signal was produced.
    pub timestamp: DateTime<Utc>,
    /// Share of the allocation to use; 0.5 when absent from the serialized form.
    #[serde(default = "default_allocation_pct")]
    pub allocation_pct: Decimal,
    /// Time until which the resulting trade may be held, if limited.
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
}

const CONSERVATIVE_HALF_ALLOCATION_PCT: Decimal = Decimal::from_parts(5, 0, 0, false, 1);

fn default_allocation_pct() -> Decimal {
    CONSERVATIVE_HALF_ALLOCATION_PCT
}

/// Trade of one account, from entry to exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    /// Identifier of the trade.
    pub id: Uuid,
    /// Identifier of the account that owns the trade.
    pub account_id: Uuid,
    /// Name of the strategy that produced the trade.
    pub strategy_name: String,
    /// Pair that was traded.
    pub pair: Pair,
    /// Exchange the trade was placed on.
    pub exchange: Exchange,
    /// Direction of the trade.
    pub direction: Direction,
    /// Price the trade was opened at.
    pub entry_price: Decimal,
    /// Price the trade was closed at, once closed.
    pub exit_price: Option<Decimal>,
    /// Stop loss price.
    pub stop_loss: Decimal,
    /// Take profit price, if any.
    pub take_profit: Option<Decimal>,
    /// Quantity of the trade.
    pub quantity: Decimal,
    /// Leverage of the trade.
    pub leverage: Decimal,
    /// Fees charged on the trade.
    pub fees: Decimal,
    /// Time the trade was opened.
    pub entry_at: DateTime<Utc>,
    /// Time the trade was closed, once closed.
    pub exit_at: Option<DateTime<Utc>>,
    /// Profit or loss amount, once closed.
    pub pnl_amount: Option<Decimal>,
    /// Reason the trade was closed, once closed.
    pub exit_reason: Option<ExitReason>,
    /// Lifecycle status of the trade.
    pub status: TradeStatus,
    /// Time until which the trade may be held, if limited.
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
    /// Identifier of the position on the exchange, if known.
    #[serde(default)]
    pub exchange_position_id: Option<String>,
    /// Identifier of the stop order on the exchange, if any.
    #[serde(default)]
    pub stop_order_id: Option<String>,
}

/// Open position, represented by its trade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    /// Trade that opened the position.
    pub trade: Trade,
}

/// Candle of one pair on one exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    /// Pair the candle belongs to.
    pub pair: Pair,
    /// Exchange the candle comes from.
    pub exchange: Exchange,
    /// Timeframe of the candle.
    pub timeframe: String,
    /// Opening price.
    pub open: Decimal,
    /// Highest price.
    pub high: Decimal,
    /// Lowest price.
    pub low: Decimal,
    /// Closing price.
    pub close: Decimal,
    /// Traded volume, if available.
    pub volume: Option<u64>,
    /// Best bid price, if available.
    #[serde(default)]
    pub best_bid: Option<Decimal>,
    /// Best ask price, if available.
    #[serde(default)]
    pub best_ask: Option<Decimal>,
    /// Time of the candle.
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod tests;
