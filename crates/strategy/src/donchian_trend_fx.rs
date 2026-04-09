//! FX 標準ブレイクアウト v1 (`donchian_trend_fx_*`).
//!
//! Pair-agnostic port of `donchian_trend_v1` tuned for OANDA FX
//! instruments on M15. Two differences from the crypto version:
//!
//! 1. **ATR-based stop loss** (`entry ± ATR × 2`, Turtle "N" stop)
//!    instead of a flat 3% distance. FX pip volatility at M15 is
//!    too small for a percentage SL to be meaningful.
//! 2. **`allocation_pct` is a constructor argument**, not a
//!    compile-time constant. The strategy is registered twice in
//!    `main.rs` (`donchian_trend_fx_normal` at 0.50 and
//!    `donchian_trend_fx_aggressive` at 0.80) so four paper
//!    accounts at two balance tiers × two risk levels can share
//!    the same code.
//!
//! ## Entry rules
//! - **Long**: current close > prior 20-bar high AND ATR(14) >
//!   rolling average ATR over the prior 50 bars.
//! - **Short**: mirror — current close < 20-bar low AND elevated
//!   ATR.
//!
//! ## Stop loss
//! `entry ± ATR(14) × 2`. Dynamic; adapts to the current volatility
//! regime so the SL is neither trivially tight in calm sessions
//! nor blown through in news events.
//!
//! ## Take profit (dynamic, via `on_open_positions`)
//! - **Long** closes when current close < prior 10-bar low.
//! - **Short** closes when current close > prior 10-bar high.
//!
//! No fixed TP — the trailing channel exit is the strategy's edge.
//!
//! ## Max hold
//! 72 hours from entry. FX trends unfold over days at M15, so
//! `max_hold_until` is set further out than the crypto version.

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy, StrategyExitReason};
use auto_trader_core::types::{Candle, Direction, Exchange, Pair, Position, Signal};
use auto_trader_market::indicators;
use chrono::Duration;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const ENTRY_CHANNEL: usize = 20;
const EXIT_CHANNEL: usize = 10;
const ATR_PERIOD: usize = 14;
const ATR_BASELINE_BARS: usize = 50;
const ATR_SL_MULT: Decimal = dec!(2.0);
const HISTORY_LEN: usize = 200;
const TIME_LIMIT_HOURS: i64 = 72;

pub struct DonchianTrendFxV1 {
    name: String,
    pairs: Vec<Pair>,
    allocation_pct: Decimal,
    history: HashMap<String, VecDeque<Candle>>,
}

impl DonchianTrendFxV1 {
    pub fn new(name: String, pairs: Vec<Pair>, allocation_pct: Decimal) -> Self {
        Self {
            name,
            pairs,
            allocation_pct,
            history: HashMap::new(),
        }
    }

    fn push_candle(&mut self, pair: &str, candle: Candle) {
        let h = self.history.entry(pair.to_string()).or_default();
        h.push_back(candle);
        while h.len() > HISTORY_LEN {
            h.pop_front();
        }
    }

    fn highs(history: &VecDeque<Candle>) -> Vec<Decimal> {
        history.iter().map(|c| c.high).collect()
    }

    fn lows(history: &VecDeque<Candle>) -> Vec<Decimal> {
        history.iter().map(|c| c.low).collect()
    }

    fn closes(history: &VecDeque<Candle>) -> Vec<Decimal> {
        history.iter().map(|c| c.close).collect()
    }

    /// Average ATR over the `ATR_BASELINE_BARS` bars prior to the
    /// current bar (current bar excluded so the breakout-day
    /// volatility doesn't pollute its own baseline). Identical to
    /// the crypto-side helper — kept local to avoid a cross-crate
    /// helper dependency that would require larger refactoring.
    fn baseline_atr(history: &VecDeque<Candle>) -> Option<Decimal> {
        if history.len() < ATR_BASELINE_BARS + ATR_PERIOD + 2 {
            return None;
        }
        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let closes = Self::closes(history);
        let latest_prior = history.len() - 2;
        let start = latest_prior + 1 - ATR_BASELINE_BARS;
        let mut sum = Decimal::ZERO;
        let mut count = 0u32;
        for end in start..=latest_prior {
            if end < ATR_PERIOD + 1 {
                continue;
            }
            if let Some(v) = indicators::atr(
                &highs[..=end],
                &lows[..=end],
                &closes[..=end],
                ATR_PERIOD,
            ) {
                sum += v;
                count += 1;
            }
        }
        if count == 0 {
            return None;
        }
        Some(sum / Decimal::from(count))
    }
}

#[async_trait::async_trait]
impl Strategy for DonchianTrendFxV1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if event.exchange != Exchange::Oanda {
            return None;
        }
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }
        let key = event.pair.0.clone();
        self.push_candle(&key, event.candle.clone());
        let history = self.history.get(&key)?;

        // Need enough history for the entry channel + ATR + baseline.
        if history.len() < ENTRY_CHANNEL + ATR_BASELINE_BARS + ATR_PERIOD + 1 {
            return None;
        }

        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let closes = Self::closes(history);

        // Entry channel uses prior bars only (current excluded).
        let (channel_low, channel_high) =
            indicators::donchian_channel(&highs, &lows, ENTRY_CHANNEL, false)?;

        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;
        let baseline = Self::baseline_atr(history)?;
        if atr <= baseline {
            // Volatility too tame — likely a false breakout.
            return None;
        }

        let entry = event.candle.close;
        let sl_offset = atr * ATR_SL_MULT;
        let max_hold = Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS));

        if entry > channel_high {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                entry_price: entry,
                stop_loss: entry - sl_offset,
                // Fixed TP parked far away — the real exit is the
                // trailing 10-bar Donchian in `on_open_positions`.
                take_profit: entry * dec!(1000),
                confidence: 0.6,
                timestamp: event.timestamp,
                allocation_pct: self.allocation_pct,
                max_hold_until: max_hold,
            });
        }
        if entry < channel_low {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                entry_price: entry,
                stop_loss: entry + sl_offset,
                take_profit: entry / dec!(1000),
                confidence: 0.6,
                timestamp: event.timestamp,
                allocation_pct: self.allocation_pct,
                max_hold_until: max_hold,
            });
        }
        None
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {}

    async fn warmup(&mut self, events: &[PriceEvent]) {
        for event in events {
            if event.exchange != Exchange::Oanda {
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
        if event.exchange != Exchange::Oanda {
            return Vec::new();
        }
        let key = event.pair.0.clone();
        let Some(history) = self.history.get(&key) else {
            return Vec::new();
        };
        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let Some((exit_low, exit_high)) =
            indicators::donchian_channel(&highs, &lows, EXIT_CHANNEL, false)
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
mod tests {
    use super::*;

    fn strat_with_alloc(alloc: Decimal) -> DonchianTrendFxV1 {
        DonchianTrendFxV1::new(
            "donchian_trend_fx_test".to_string(),
            vec![Pair::new("USD_JPY")],
            alloc,
        )
    }

    #[test]
    fn constructor_stores_allocation_pct() {
        let s = strat_with_alloc(dec!(0.5));
        assert_eq!(s.allocation_pct, dec!(0.5));

        let s = strat_with_alloc(dec!(0.8));
        assert_eq!(s.allocation_pct, dec!(0.8));
    }

    #[test]
    fn baseline_atr_requires_minimum_history() {
        let s = strat_with_alloc(dec!(0.5));
        let mut history: VecDeque<Candle> = VecDeque::new();
        // Add exactly ATR_BASELINE_BARS + ATR_PERIOD + 1 bars — one
        // short of the guard. Must return None.
        let total = ATR_BASELINE_BARS + ATR_PERIOD + 1;
        for i in 0..total {
            history.push_back(Candle {
                pair: Pair::new("USD_JPY"),
                exchange: Exchange::Oanda,
                timeframe: "M15".to_string(),
                open: dec!(150.0),
                high: dec!(150.05),
                low: dec!(149.95),
                close: dec!(150.00) + Decimal::from(i as i64) / dec!(100),
                volume: Some(100),
                timestamp: chrono::Utc::now(),
            });
        }
        assert!(DonchianTrendFxV1::baseline_atr(&history).is_none());
        let _ = s; // avoid unused warning
    }
}
