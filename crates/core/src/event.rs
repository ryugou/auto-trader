use crate::types::{Candle, ExitReason, Pair, Signal, Trade};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PriceEvent {
    pub pair: Pair,
    pub candle: Candle,
    pub indicators: HashMap<String, Decimal>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SignalEvent {
    pub signal: Signal,
}

#[derive(Debug, Clone)]
pub enum TradeAction {
    Opened,
    Closed { exit_price: Decimal, exit_reason: ExitReason },
}

#[derive(Debug, Clone)]
pub struct TradeEvent {
    pub trade: Trade,
    pub action: TradeAction,
}
