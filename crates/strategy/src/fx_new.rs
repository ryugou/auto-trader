use std::collections::{HashMap, VecDeque};

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy, StrategyExitReason};
use auto_trader_core::types::{Candle, Direction, Exchange, Pair, Position, Signal};
use auto_trader_market::indicators;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const ENTRY_CHANNEL: usize = 16;
const EXIT_CHANNEL: usize = 8;
const M15_MINUTES: i64 = 15;
const M15_SECONDS: i64 = M15_MINUTES * 60;
const ATR_PERIOD: usize = 14;
const ATR_BASELINE: usize = 20;
const H1_FAST: usize = 20;
const H1_SLOW: usize = 50;
const ATR_MULT: Decimal = dec!(2);
const SL_CAP: Decimal = dec!(0.02);
const TARGET_RISK_PCT: Decimal = dec!(0.01);
const SIGNAL_CONFIDENCE: f64 = 0.65;
/// Leverage that the strategy assumes for GMO FX.
pub const GMO_FX_LEVERAGE: Decimal = dec!(10);
/// Exchange whose price events the strategy accepts.
pub const FX_NEW_EXCHANGE: Exchange = Exchange::GmoFx;
const M15_BARS_PER_TRADING_DAY: usize = 96;
const HISTORY_TRADING_DAYS: usize = 5;
const HISTORY_LEN: usize = M15_BARS_PER_TRADING_DAY * HISTORY_TRADING_DAYS;

/// Breakout strategy that builds M15 candles from M5 candles and trades in the direction of the H1 moving average trend.
pub struct FxNewV1 {
    name: String,
    pairs: Vec<Pair>,
    m5_pending: HashMap<String, Vec<Candle>>,
    m15: HashMap<String, VecDeque<Candle>>,
    h1: HashMap<String, VecDeque<Candle>>,
}

impl FxNewV1 {
    /// Creates the strategy under the given name for the given pairs.
    pub fn new(name: String, pairs: Vec<Pair>) -> Self {
        Self {
            name,
            pairs,
            m5_pending: HashMap::new(),
            m15: HashMap::new(),
            h1: HashMap::new(),
        }
    }

    fn accepts(&self, event: &PriceEvent) -> bool {
        event.exchange == FX_NEW_EXCHANGE && self.pairs.iter().any(|p| p == &event.pair)
    }

    fn push_limited(history: &mut VecDeque<Candle>, candle: Candle) {
        history.push_back(candle);
        while history.len() > HISTORY_LEN {
            history.pop_front();
        }
    }

    fn is_valid_candle(candle: &Candle) -> bool {
        candle.open > Decimal::ZERO
            && candle.high > Decimal::ZERO
            && candle.low > Decimal::ZERO
            && candle.close > Decimal::ZERO
    }

    fn log_invalid_candle(context: &'static str, pair: &Pair, candle: &Candle) {
        tracing::warn!(
            context = context,
            pair = %pair,
            timestamp = %candle.timestamp,
            open = %candle.open,
            high = %candle.high,
            low = %candle.low,
            close = %candle.close,
            "invalid candle: non-positive ohlc"
        );
    }

    fn push_h1(&mut self, event: &PriceEvent) {
        if !Self::is_valid_candle(&event.candle) {
            Self::log_invalid_candle("h1_candle", &event.pair, &event.candle);
            return;
        }
        let history = self.h1.entry(event.pair.0.clone()).or_default();
        Self::push_limited(history, event.candle.clone());
    }

    fn aggregate_m15(&mut self, event: &PriceEvent) -> Option<Candle> {
        let ts = event.candle.timestamp.timestamp();
        let bucket = ts - ts.rem_euclid(M15_SECONDS);
        let pending = self.m5_pending.entry(event.pair.0.clone()).or_default();

        if pending.first().is_some_and(|c| {
            c.timestamp.timestamp() - c.timestamp.timestamp().rem_euclid(M15_SECONDS) != bucket
        }) {
            pending.clear();
        }
        if pending
            .iter()
            .any(|c| c.timestamp == event.candle.timestamp)
        {
            return None;
        }
        pending.push(event.candle.clone());
        pending.sort_by_key(|c| c.timestamp);
        if pending.len() != 3 {
            return None;
        }

        let first = pending.first()?.clone();
        let last = pending.last()?.clone();
        let high = pending.iter().map(|c| c.high).max()?;
        let low = pending.iter().map(|c| c.low).min()?;
        let volume = Some(pending.iter().map(|c| c.volume.unwrap_or(0)).sum());
        pending.clear();
        Some(Candle {
            pair: first.pair,
            exchange: first.exchange,
            timeframe: "M15".to_string(),
            open: first.open,
            high,
            low,
            close: last.close,
            volume,
            best_bid: last.best_bid,
            best_ask: last.best_ask,
            timestamp: chrono::DateTime::from_timestamp(bucket, 0)?,
        })
    }

    fn h1_direction(&self, pair: &str) -> Option<Direction> {
        let closes: Vec<Decimal> = self.h1.get(pair)?.iter().map(|c| c.close).collect();
        let fast = indicators::sma(&closes, H1_FAST)?;
        let slow = indicators::sma(&closes, H1_SLOW)?;
        if fast > slow {
            Some(Direction::Long)
        } else if fast < slow {
            Some(Direction::Short)
        } else {
            None
        }
    }

    fn allocation_for(stop_loss_pct: Decimal) -> Decimal {
        (TARGET_RISK_PCT / (GMO_FX_LEVERAGE * stop_loss_pct)).min(Decimal::ONE)
    }

    fn m15_end(candle: &Candle) -> chrono::DateTime<chrono::Utc> {
        candle.timestamp + chrono::Duration::minutes(M15_MINUTES)
    }

    fn reached_one_r(position: &Position, history: &VecDeque<Candle>, current: &Candle) -> bool {
        let trade = &position.trade;
        let one_r = match trade.direction {
            Direction::Long => trade.entry_price - trade.stop_loss,
            Direction::Short => trade.stop_loss - trade.entry_price,
        };
        if one_r <= Decimal::ZERO {
            return false;
        }
        let target = match trade.direction {
            Direction::Long => trade.entry_price + one_r,
            Direction::Short => trade.entry_price - one_r,
        };
        history
            .iter()
            .filter(|c| Self::m15_end(c) > trade.entry_at)
            .chain(std::iter::once(current))
            .any(|c| match trade.direction {
                Direction::Long => c.high >= target,
                Direction::Short => c.low <= target,
            })
    }
}

#[async_trait::async_trait]
impl Strategy for FxNewV1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if !self.accepts(event) {
            return None;
        }
        if event.candle.timeframe == "H1" {
            self.push_h1(event);
            return None;
        }
        if event.candle.timeframe != "M5" {
            return None;
        }
        if !Self::is_valid_candle(&event.candle) {
            Self::log_invalid_candle("m5_candle_before_aggregation", &event.pair, &event.candle);
            return None;
        }
        let candle = self.aggregate_m15(event)?;
        if !Self::is_valid_candle(&candle) {
            Self::log_invalid_candle("m15_aggregate_candle", &candle.pair, &candle);
            return None;
        }
        let key = event.pair.0.clone();
        let history = self.m15.entry(key.clone()).or_default();
        Self::push_limited(history, candle.clone());
        if history.len() < ENTRY_CHANNEL + ATR_BASELINE + ATR_PERIOD + 1 {
            return None;
        }

        let highs: Vec<Decimal> = history.iter().map(|c| c.high).collect();
        let lows: Vec<Decimal> = history.iter().map(|c| c.low).collect();
        let closes: Vec<Decimal> = history.iter().map(|c| c.close).collect();
        let (channel_low, channel_high) =
            indicators::donchian_channel(&highs, &lows, ENTRY_CHANNEL, false)?;
        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;

        let prior_end = history.len() - 1;
        let baseline_start = prior_end.saturating_sub(ATR_BASELINE + ATR_PERIOD);
        let mut atr_sum = Decimal::ZERO;
        let mut atr_count = Decimal::ZERO;
        for end in (baseline_start + ATR_PERIOD + 1)..=prior_end {
            if let Some(value) = indicators::atr(
                &highs[baseline_start..end],
                &lows[baseline_start..end],
                &closes[baseline_start..end],
                ATR_PERIOD,
            ) {
                atr_sum += value;
                atr_count += Decimal::ONE;
            }
        }
        if atr_count == Decimal::ZERO || atr <= atr_sum / atr_count {
            return None;
        }

        let direction = self.h1_direction(&key)?;
        let entry = candle.close;
        let breakout = match direction {
            Direction::Long => entry > channel_high,
            Direction::Short => entry < channel_low,
        };
        if !breakout {
            return None;
        }
        let stop_loss_pct = (atr * ATR_MULT / entry).min(SL_CAP);
        if stop_loss_pct <= Decimal::ZERO {
            return None;
        }
        Some(Signal {
            strategy_name: self.name.clone(),
            pair: event.pair.clone(),
            direction,
            stop_loss_pct,
            take_profit_pct: None,
            confidence: SIGNAL_CONFIDENCE,
            timestamp: candle.timestamp,
            allocation_pct: Self::allocation_for(stop_loss_pct),
            max_hold_until: None,
        })
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {}

    async fn warmup(&mut self, events: &[PriceEvent]) {
        for event in events {
            if !self.accepts(event) {
                continue;
            }
            if event.candle.timeframe == "H1" {
                self.push_h1(event);
            } else if event.candle.timeframe == "M5" && !Self::is_valid_candle(&event.candle) {
                Self::log_invalid_candle(
                    "m5_candle_before_aggregation_warmup",
                    &event.pair,
                    &event.candle,
                );
            } else if event.candle.timeframe == "M5"
                && let Some(candle) = self.aggregate_m15(event)
            {
                if Self::is_valid_candle(&candle) {
                    let history = self.m15.entry(event.pair.0.clone()).or_default();
                    Self::push_limited(history, candle);
                } else {
                    Self::log_invalid_candle("m15_aggregate_candle_warmup", &candle.pair, &candle);
                }
            }
        }
    }

    async fn on_open_positions(
        &mut self,
        positions: &[Position],
        event: &PriceEvent,
    ) -> Vec<ExitSignal> {
        if event.candle.timeframe != "M5" || !self.accepts(event) {
            return Vec::new();
        }
        if !Self::is_valid_candle(&event.candle) {
            Self::log_invalid_candle("m5_candle_for_exit_check", &event.pair, &event.candle);
            return Vec::new();
        }
        let Some(history) = self.m15.get(&event.pair.0) else {
            return Vec::new();
        };
        let (highs, lows): (Vec<Decimal>, Vec<Decimal>) = history
            .iter()
            .filter(|c| Self::m15_end(c) <= event.candle.timestamp)
            .map(|c| (c.high, c.low))
            .unzip();
        let Some((exit_low, exit_high)) =
            indicators::donchian_channel(&highs, &lows, EXIT_CHANNEL, true)
        else {
            return Vec::new();
        };
        let close = event.candle.close;
        positions
            .iter()
            .filter(|p| p.trade.strategy_name == self.name && p.trade.pair == event.pair)
            .filter(|p| Self::reached_one_r(p, history, &event.candle))
            .filter(|p| match p.trade.direction {
                Direction::Long => close < exit_low,
                Direction::Short => close > exit_high,
            })
            .map(|p| ExitSignal {
                trade_id: p.trade.id,
                reason: StrategyExitReason::TrailingChannel,
                close_price: close,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
