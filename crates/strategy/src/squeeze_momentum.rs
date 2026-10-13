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
const BB_STDDEV: Decimal = dec!(2);
const KC_PERIOD: usize = 20;
const KC_ATR_MULT: Decimal = dec!(1.5);
const ATR_PERIOD: usize = 14;
const CHANDELIER_PERIOD: usize = 22;
const CHANDELIER_ATR_MULT: Decimal = dec!(3.0);
const DELAY_BARS: usize = 3;
const SQUEEZE_BARS: usize = 3;
const SWING_LOOKBACK: usize = 5;
const ATR_MULT: Decimal = dec!(2.5);
const SL_CAP: Decimal = dec!(0.05);
const ALLOCATION_CAP: Decimal = dec!(1.00);
const TIME_LIMIT_HOURS: i64 = 48;
const HISTORY_LEN: usize = 200;
const TIMEFRAME: &str = "H1";

pub struct SqueezeMomentumV1 {
    name: String,
    pairs: Vec<Pair>,
    history: HashMap<String, VecDeque<Candle>>,
    squeeze_count: HashMap<String, usize>,
}

impl SqueezeMomentumV1 {
    pub fn new(name: String, pairs: Vec<Pair>) -> Self {
        Self {
            name,
            pairs,
            history: HashMap::new(),
            squeeze_count: HashMap::new(),
        }
    }

    fn push_candle(&mut self, pair: &str, candle: Candle) {
        let h = self.history.entry(pair.to_string()).or_default();
        candle_history::push_candle(h, candle, HISTORY_LEN);
    }

    fn record_squeeze_bar(squeeze_count: &mut HashMap<String, usize>, key: &str, in_squeeze: bool) {
        let prev = *squeeze_count.get(key).unwrap_or(&0);
        let new_count = if in_squeeze { prev + 1 } else { 0 };
        squeeze_count.insert(key.to_string(), new_count);
    }

    fn floor_to_hour(ts: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
        use chrono::Timelike;
        ts.with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
    }

    fn is_in_squeeze(history: &VecDeque<Candle>) -> Option<bool> {
        let closes = candle_history::closes(history);
        let highs = candle_history::highs(history);
        let lows = candle_history::lows(history);
        let (bb_lo, _, bb_up) = indicators::bollinger_bands(&closes, BB_PERIOD, BB_STDDEV)?;
        let (kc_lo, _, kc_up) =
            indicators::keltner_channels(&highs, &lows, &closes, KC_PERIOD, KC_ATR_MULT)?;
        Some(bb_lo >= kc_lo && bb_up <= kc_up)
    }
}

#[async_trait::async_trait]
impl Strategy for SqueezeMomentumV1 {
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

        let needed = BB_PERIOD
            .max(KC_PERIOD)
            .max(ATR_PERIOD + 1)
            .max(CHANDELIER_PERIOD)
            .max(SWING_LOOKBACK + 1);
        let history = self.history.get(&key)?;
        if history.len() < needed + 2 {
            return None;
        }

        let in_squeeze = Self::is_in_squeeze(history)?;
        let prev_count = *self.squeeze_count.get(&key).unwrap_or(&0);
        Self::record_squeeze_bar(&mut self.squeeze_count, &key, in_squeeze);

        let just_fired = !in_squeeze && prev_count >= SQUEEZE_BARS;
        if !just_fired {
            return None;
        }

        let closes = candle_history::closes(history);
        let highs = candle_history::highs(history);
        let lows = candle_history::lows(history);

        let sma20 = indicators::sma(&closes, BB_PERIOD)?;
        let mom_curr = closes[closes.len() - 1] - sma20;
        let sma20_prev = indicators::sma(&closes[..closes.len() - 1], BB_PERIOD)?;
        let mom_prev = closes[closes.len() - 2] - sma20_prev;

        let entry = event.candle.close;
        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;
        let stop_loss_pct = (atr * ATR_MULT / entry).min(SL_CAP);
        if stop_loss::lacks_tradable_volatility(stop_loss_pct) {
            return None;
        }
        let allocation_pct = ALLOCATION_CAP;

        if mom_curr > Decimal::ZERO && mom_curr > mom_prev {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                stop_loss_pct,
                take_profit_pct: None,
                confidence: 0.55,
                timestamp: event.timestamp,
                allocation_pct,
                max_hold_until: Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS)),
            });
        }
        if mom_curr < Decimal::ZERO && mom_curr < mom_prev {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                stop_loss_pct,
                take_profit_pct: None,
                confidence: 0.55,
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
            if let Some(history) = self.history.get(&event.pair.0)
                && let Some(in_squeeze) = Self::is_in_squeeze(history)
            {
                Self::record_squeeze_bar(&mut self.squeeze_count, &event.pair.0, in_squeeze);
            }
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
        let closes = candle_history::closes(history);
        let close = event.candle.close;

        let Some(atr) = indicators::atr(&highs, &lows, &closes, ATR_PERIOD) else {
            return Vec::new();
        };
        if atr <= Decimal::ZERO {
            return Vec::new();
        }

        if highs.len() < CHANDELIER_PERIOD || lows.len() < CHANDELIER_PERIOD {
            return Vec::new();
        }
        let recent_highs = &highs[highs.len() - CHANDELIER_PERIOD..];
        let recent_lows = &lows[lows.len() - CHANDELIER_PERIOD..];
        let highest_high = recent_highs.iter().copied().max().unwrap_or(close);
        let lowest_low = recent_lows.iter().copied().min().unwrap_or(close);

        let chandelier_offset = atr * CHANDELIER_ATR_MULT;

        let mut exits = Vec::new();
        for pos in positions {
            if pos.trade.strategy_name != self.name {
                continue;
            }
            if pos.trade.pair.0 != key {
                continue;
            }

            let entry_hour = Self::floor_to_hour(pos.trade.entry_at);
            let bars_held = history
                .iter()
                .filter(|c| c.timestamp >= entry_hour)
                .count()
                .saturating_sub(1);
            if bars_held < DELAY_BARS {
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

            let trail_break = match pos.trade.direction {
                Direction::Long => {
                    let stop = highest_high - chandelier_offset;
                    close < stop
                }
                Direction::Short => {
                    let stop = lowest_low + chandelier_offset;
                    close > stop
                }
            };
            if trail_break {
                exits.push(ExitSignal {
                    trade_id: pos.trade.id,
                    reason: StrategyExitReason::TrailingMa,
                    close_price: close,
                });
            }
        }
        exits
    }
}

#[cfg(test)]
mod tests;
