use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::types::{Candle, Exchange, ExitReason, Pair, Signal, Trade};

#[derive(Debug, Clone)]
pub struct PriceEvent {
    pub pair: Pair,
    pub exchange: Exchange,
    pub candle: Candle,
    pub indicators: HashMap<String, Decimal>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SignalEvent {
    pub signal: Signal,
    pub indicators: HashMap<String, Decimal>,
}

#[derive(Debug, Clone)]
pub enum TradeAction {
    Opened,
    Closed {
        exit_price: Decimal,
        exit_reason: ExitReason,
    },
}

#[derive(Debug, Clone)]
pub struct TradeEvent {
    pub trade: Trade,
    pub action: TradeAction,
    pub account_type: Option<String>,
}
