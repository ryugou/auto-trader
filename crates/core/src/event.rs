use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::types::{Candle, Exchange, ExitReason, Pair, Signal, Trade};

/// Price update of one pair on one exchange.
#[derive(Debug, Clone)]
pub struct PriceEvent {
    /// Pair the price belongs to.
    pub pair: Pair,
    /// Exchange the price belongs to.
    pub exchange: Exchange,
    /// Candle that carries the price.
    pub candle: Candle,
    /// Indicator values keyed by name.
    pub indicators: HashMap<String, Decimal>,
    /// Time of the event.
    pub timestamp: DateTime<Utc>,
}

/// Trading signal together with the indicator values attached to it.
#[derive(Debug, Clone)]
pub struct SignalEvent {
    /// The trading signal.
    pub signal: Signal,
    /// Indicator values keyed by name.
    pub indicators: HashMap<String, Decimal>,
}

/// Change of a trade that a trade event reports.
#[derive(Debug, Clone)]
pub enum TradeAction {
    /// The trade was opened.
    Opened,
    /// The trade was closed.
    Closed {
        /// Price the trade was closed at.
        exit_price: Decimal,
        /// Reason the trade was closed.
        exit_reason: ExitReason,
    },
}

/// Event that reports an opened or closed trade.
#[derive(Debug, Clone)]
pub struct TradeEvent {
    /// The trade the event is about.
    pub trade: Trade,
    /// What happened to the trade.
    pub action: TradeAction,
    /// Type of the account that owns the trade, if set.
    pub account_type: Option<String>,
}
