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
}

impl Exchange {
    pub fn as_str(&self) -> &'static str {
        match self {
            Exchange::Oanda => "oanda",
            Exchange::BitflyerCfd => "bitflyer_cfd",
        }
    }
}

impl std::fmt::Display for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Long,
    Short,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeMode {
    Live,
    Paper,
    Backtest,
}

impl TradeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            TradeMode::Live => "live",
            TradeMode::Paper => "paper",
            TradeMode::Backtest => "backtest",
        }
    }
}

impl std::fmt::Display for TradeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    TpHit,
    SlHit,
    Manual,
    SignalReverse,
    /// Mean-reversion target reached (e.g. price returned to BB middle).
    /// Strategy-driven exit emitted via `Strategy::on_open_positions`.
    StrategyMeanReached,
    /// Trailing channel break (e.g. Donchian opposite-side break).
    StrategyTrailingChannel,
    /// Trailing moving-average break (e.g. close beyond EMA(21)).
    StrategyTrailingMa,
    /// Indicator-based reversal (e.g. RSI crossed back through midline).
    StrategyIndicatorReversal,
    /// Time-based fail-safe — `max_hold_until` deadline reached.
    StrategyTimeLimit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
    /// Optional time-based fail-safe: position monitor will force-close
    /// the trade at this UTC time even if neither SL nor TP nor any
    /// strategy-driven exit has fired. Strategies use this to bound
    /// "stale" trades (e.g. mean-reversion 24h, vol-breakout 48h).
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: Uuid,
    pub strategy_name: String,
    pub pair: Pair,
    pub exchange: Exchange,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub quantity: Option<Decimal>,
    pub leverage: Decimal,
    pub fees: Decimal,
    pub paper_account_id: Option<Uuid>,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_pips: Option<Decimal>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<ExitReason>,
    pub mode: TradeMode,
    pub status: TradeStatus,
    /// Optional time-based fail-safe — see `Signal::max_hold_until`.
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
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
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn pair_display_format() {
        let pair = Pair::new("USD_JPY");
        assert_eq!(pair.to_string(), "USD_JPY");
    }

    #[test]
    fn direction_serializes_snake_case() {
        let json = serde_json::to_string(&Direction::Long).unwrap();
        assert_eq!(json, r#""long""#);
    }

    #[test]
    fn exchange_serializes_snake_case() {
        let json = serde_json::to_string(&Exchange::BitflyerCfd).unwrap();
        assert_eq!(json, r#""bitflyer_cfd""#);
        let json = serde_json::to_string(&Exchange::Oanda).unwrap();
        assert_eq!(json, r#""oanda""#);
    }

    #[test]
    fn exchange_display() {
        assert_eq!(Exchange::Oanda.as_str(), "oanda");
        assert_eq!(Exchange::BitflyerCfd.as_str(), "bitflyer_cfd");
    }

    #[test]
    fn signal_roundtrip() {
        let signal = Signal {
            strategy_name: "test".to_string(),
            pair: Pair::new("USD_JPY"),
            direction: Direction::Long,
            entry_price: dec!(150.00),
            stop_loss: dec!(149.50),
            take_profit: dec!(151.00),
            confidence: 0.8,
            timestamp: Utc::now(),
            max_hold_until: None,
        };
        let json = serde_json::to_string(&signal).unwrap();
        let back: Signal = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pair, signal.pair);
        assert_eq!(back.direction, Direction::Long);
    }
}
