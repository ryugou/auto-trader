use std::collections::VecDeque;

use auto_trader_core::types::Candle;
use rust_decimal::Decimal;

pub(crate) fn push_candle(history: &mut VecDeque<Candle>, candle: Candle, max_len: usize) {
    history.push_back(candle);
    while history.len() > max_len {
        history.pop_front();
    }
}

pub(crate) fn highs(history: &VecDeque<Candle>) -> Vec<Decimal> {
    history.iter().map(|c| c.high).collect()
}

pub(crate) fn lows(history: &VecDeque<Candle>) -> Vec<Decimal> {
    history.iter().map(|c| c.low).collect()
}

pub(crate) fn closes(history: &VecDeque<Candle>) -> Vec<Decimal> {
    history.iter().map(|c| c.close).collect()
}
