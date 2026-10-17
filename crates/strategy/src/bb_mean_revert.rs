use std::collections::{HashMap, VecDeque};

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{
    ExitSignal, MacroUpdate, Strategy, StrategyExitReason, has_reached_one_r,
};
use auto_trader_core::types::{Candle, Direction, Pair, Position, Signal};
use auto_trader_market::indicators;
use chrono::Duration;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::candle_history;
use crate::stop_loss;

const BB_PERIOD: usize = 20;
const RSI_PERIOD: usize = 14;
const ATR_PERIOD: usize = 14;
const HISTORY_LEN: usize = 200;

const RSI_LONG_THRESHOLD: Decimal = dec!(25);
const RSI_SHORT_THRESHOLD: Decimal = dec!(75);
const ATR_MULT: Decimal = dec!(1.5);
const SL_CAP: Decimal = dec!(0.03);
const ALLOCATION_CAP: Decimal = dec!(1.00);
const TIME_LIMIT_HOURS: i64 = 24;
const TIMEFRAME: &str = "M5";
const BB_STD_DEV_MULT: Decimal = dec!(2.5);

/// Mean reversion strategy that trades M5 candles closing outside the Bollinger Bands when RSI is extreme.
pub struct BbMeanRevertV1 {
    name: String,
    pairs: Vec<Pair>,
    history: HashMap<String, VecDeque<Candle>>,
}

impl BbMeanRevertV1 {
    /// Creates the strategy under the given name for the given pairs.
    pub fn new(name: String, pairs: Vec<Pair>) -> Self {
        Self {
            name,
            pairs,
            history: HashMap::new(),
        }
    }

    fn push_candle(&mut self, pair: &str, candle: Candle) {
        let h = self.history.entry(pair.to_string()).or_default();
        candle_history::push_candle(h, candle, HISTORY_LEN);
    }

    fn min_history_len_for_signal() -> usize {
        BB_PERIOD.max(RSI_PERIOD + 1).max(ATR_PERIOD + 1) + 1
    }
}

#[async_trait::async_trait]
impl Strategy for BbMeanRevertV1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if event.candle.timeframe != TIMEFRAME {
            return None;
        }
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }
        let key = event.pair.0.clone();
        self.push_candle(&key, event.candle.clone());
        let history = self.history.get(&key)?;

        if history.len() < Self::min_history_len_for_signal() {
            return None;
        }

        let closes = candle_history::closes(history);
        let highs = candle_history::highs(history);
        let lows = candle_history::lows(history);

        let (lower, _middle, upper) =
            indicators::bollinger_bands(&closes, BB_PERIOD, BB_STD_DEV_MULT)?;
        let rsi = indicators::rsi(&closes, RSI_PERIOD)?;
        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;
        let entry = event.candle.close;

        let stop_loss_pct = (atr * ATR_MULT / entry).min(SL_CAP);
        if stop_loss::lacks_tradable_volatility(stop_loss_pct) {
            return None;
        }
        let allocation_pct = ALLOCATION_CAP;

        let len = history.len();
        let prev_candle = &history[len - 2];
        let curr_candle = &history[len - 1];

        if entry < lower && rsi < RSI_LONG_THRESHOLD && curr_candle.low < prev_candle.low {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                stop_loss_pct,
                take_profit_pct: None,
                confidence: 0.65,
                timestamp: event.timestamp,
                allocation_pct,
                max_hold_until: Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS)),
            });
        }

        if entry > upper && rsi > RSI_SHORT_THRESHOLD && curr_candle.high > prev_candle.high {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                stop_loss_pct,
                take_profit_pct: None,
                confidence: 0.65,
                timestamp: event.timestamp,
                allocation_pct,
                max_hold_until: Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS)),
            });
        }

        None
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {}

    async fn warmup(&mut self, events: &[PriceEvent]) {
        for event in events {
            if event.candle.timeframe != TIMEFRAME {
                continue;
            }
            if !self.pairs.iter().any(|p| p == &event.pair) {
                continue;
            }
            self.push_candle(&event.pair.0, event.candle.clone());
        }
    }

    async fn on_open_positions(
        &mut self,
        positions: &[Position],
        event: &PriceEvent,
    ) -> Vec<ExitSignal> {
        if event.candle.timeframe != TIMEFRAME {
            return Vec::new();
        }
        let key = event.pair.0.clone();
        let Some(history) = self.history.get(&key) else {
            return Vec::new();
        };
        if history.is_empty() {
            return Vec::new();
        }

        let closes = candle_history::closes(history);
        let Some(middle) = indicators::sma(&closes, BB_PERIOD) else {
            return Vec::new();
        };

        let mut exits = Vec::new();
        let close = event.candle.close;
        for pos in positions {
            if pos.trade.strategy_name != self.name {
                continue;
            }
            if pos.trade.pair.0 != key {
                continue;
            }

            if !has_reached_one_r(
                &pos.trade.direction,
                pos.trade.entry_price,
                pos.trade.stop_loss,
                close,
            ) {
                continue;
            }

            let mean_reached = match pos.trade.direction {
                Direction::Long => close >= middle,
                Direction::Short => close <= middle,
            };
            if mean_reached {
                exits.push(ExitSignal {
                    trade_id: pos.trade.id,
                    reason: StrategyExitReason::MeanReached,
                    close_price: close,
                });
            }
        }
        exits
    }
}

#[cfg(test)]
mod tests;
