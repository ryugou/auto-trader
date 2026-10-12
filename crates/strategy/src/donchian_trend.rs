use std::collections::{HashMap, VecDeque};

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{
    ExitSignal, MacroUpdate, Strategy, StrategyExitReason, has_reached_one_r,
};
use auto_trader_core::types::{Candle, Direction, Pair, Position, Signal};
use auto_trader_market::indicators;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::candle_history;
use crate::donchian_baseline;
use crate::stop_loss;

const ENTRY_CHANNEL: usize = 20;
const EXIT_CHANNEL: usize = 10;
const ATR_PERIOD: usize = 14;
const ATR_BASELINE_BARS: usize = 20;
const ATR_MULT: Decimal = dec!(3.0);
const SL_CAP: Decimal = dec!(0.05);
const ALLOCATION_CAP: Decimal = dec!(1.00);
const HISTORY_LEN: usize = 200;
const TIMEFRAME: &str = "H1";

pub struct DonchianTrendV1 {
    name: String,
    pairs: Vec<Pair>,
    history: HashMap<String, VecDeque<Candle>>,
}

impl DonchianTrendV1 {
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
}

#[async_trait::async_trait]
impl Strategy for DonchianTrendV1 {
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

        if history.len()
            < donchian_baseline::min_history_len_for_entry(
                ENTRY_CHANNEL,
                ATR_BASELINE_BARS,
                ATR_PERIOD,
            )
        {
            return None;
        }

        let highs = candle_history::highs(history);
        let lows = candle_history::lows(history);
        let closes = candle_history::closes(history);

        let (channel_low, channel_high) =
            donchian_baseline::channel_excluding_current_bar(&highs, &lows, ENTRY_CHANNEL)?;

        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;
        let baseline = donchian_baseline::baseline_atr_excluding_current_bar(
            history,
            ATR_BASELINE_BARS,
            ATR_PERIOD,
        )?;
        if atr <= baseline {
            return None;
        }

        let entry = event.candle.close;
        let stop_loss_pct = (atr * ATR_MULT / entry).min(SL_CAP);
        if stop_loss::lacks_tradable_volatility(stop_loss_pct) {
            return None;
        }
        let allocation_pct = ALLOCATION_CAP;

        if entry > channel_high {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                stop_loss_pct,
                take_profit_pct: None,
                confidence: 0.6,
                timestamp: event.timestamp,
                allocation_pct,
                max_hold_until: None,
            });
        }
        if entry < channel_low {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                stop_loss_pct,
                take_profit_pct: None,
                confidence: 0.6,
                timestamp: event.timestamp,
                allocation_pct,
                max_hold_until: None,
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
        let highs = candle_history::highs(history);
        let lows = candle_history::lows(history);
        let Some((exit_low, exit_high)) =
            donchian_baseline::channel_excluding_current_bar(&highs, &lows, EXIT_CHANNEL)
        else {
            return Vec::new();
        };

        let close = event.candle.close;
        let mut exits = Vec::new();
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

            let trailing_break = match pos.trade.direction {
                Direction::Long => close < exit_low,
                Direction::Short => close > exit_high,
            };
            if trailing_break {
                exits.push(ExitSignal {
                    trade_id: pos.trade.id,
                    reason: StrategyExitReason::TrailingChannel,
                    close_price: close,
                });
            }
        }
        exits
    }
}

#[cfg(test)]
mod tests;
