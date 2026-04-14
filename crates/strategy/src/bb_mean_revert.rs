//! 慎重平均回帰 v1 (`bb_mean_revert_v1`).
//!
//! Bollinger Bands mean-reversion strategy designed for ranging crypto
//! markets, derived from the classic [Babypips Short-Term Bollinger
//! Reversion](https://www.babypips.com/trading/system-rules-short-term-bollinger-reversion-strategy)
//! ruleset and adapted for FX_BTC_JPY (M5).
//!
//! ## Entry rules
//! - **Long**: close < BB(20, 2.5σ) lower AND RSI(14) < 25 AND the previous
//!   bar made a lower-low (capitulation confirmation).
//! - **Short**: mirror — close > upper AND RSI > 75 AND previous bar made
//!   a higher-high.
//!
//! ## Stop loss
//! Flat **2 % from entry price** (`SL_PCT`). Mean-reversion entries
//! are tight by design — if the reversion thesis fails, get out at -2 %.
//! Sizing is decoupled from the SL distance via `allocation_pct` on
//! the emitted Signal, so the SL value is purely a price level for
//! the position monitor to enforce.
//!
//! ## Take profit (dynamic, via `on_open_positions`)
//! - **Long** closes when price returns to SMA20 (BB middle).
//! - **Short** closes when price returns to SMA20.
//! - Time-limit fail-safe: any trade older than 24h is force-closed by
//!   the position monitor via `max_hold_until`.
//!
//! Invalidation is left entirely to the SL — there's no separate "RSI
//! reversal" rule because the natural mean-reversion exit is already
//! the SMA20 touch, and a wider RSI move *toward* the mean is exactly
//! what we want, not a sign to bail out early.
//!
//! Risk profile: low ("慎重"). Targets ranging conditions, expects ~60%
//! win rate at R:R ~1:1.2.

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy, StrategyExitReason};
use auto_trader_core::types::{Candle, Direction, Exchange, OrderType, Pair, Position, Signal};
use auto_trader_market::indicators;
use chrono::Duration;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const BB_PERIOD: usize = 20;
const RSI_PERIOD: usize = 14;
/// Number of historical candles each per-pair `VecDeque` keeps. Enough
/// for BB(20) + RSI(14+1) + the "previous bar lower-low" check, with
/// some headroom for the warmup-from-DB seed (200 bars in main.rs).
const HISTORY_LEN: usize = 200;

const RSI_LONG_THRESHOLD: Decimal = dec!(25);
const RSI_SHORT_THRESHOLD: Decimal = dec!(75);
/// Stop-loss as a flat percentage of entry price. Mean-reversion entries
/// are tight by design — if the reversion thesis fails, get out at -2 %.
const SL_PCT: Decimal = dec!(0.02);
/// Capital allocation per trade. Each strategy runs on its own
/// dedicated paper account with no expected concurrent positions, so
/// leaving cash idle wastes the experiment. 100 % is safe at 2×
/// leverage + a 2 % SL because the SL fires well before the
/// maintenance-margin threshold, so the exchange can't force-close
/// before our own SL does.
const ALLOCATION_PCT: Decimal = dec!(1.00);
const TIME_LIMIT_HOURS: i64 = 24;

pub struct BbMeanRevertV1 {
    name: String,
    pairs: Vec<Pair>,
    history: HashMap<String, VecDeque<Candle>>,
}

impl BbMeanRevertV1 {
    pub fn new(name: String, pairs: Vec<Pair>) -> Self {
        Self {
            name,
            pairs,
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

    fn closes(history: &VecDeque<Candle>) -> Vec<Decimal> {
        history.iter().map(|c| c.close).collect()
    }
}

#[async_trait::async_trait]
impl Strategy for BbMeanRevertV1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if event.exchange != Exchange::BitflyerCfd {
            return None;
        }
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }
        let key = event.pair.0.clone();
        self.push_candle(&key, event.candle.clone());
        let history = self.history.get(&key)?;

        // Need at least BB_PERIOD candles plus one previous bar for the
        // capitulation check. RSI needs its own history too.
        if history.len() < BB_PERIOD.max(RSI_PERIOD + 1) + 1 {
            return None;
        }

        let closes = Self::closes(history);
        let (lower, _middle, upper) = indicators::bollinger_bands(&closes, BB_PERIOD, dec!(2.5))?;
        let rsi = indicators::rsi(&closes, RSI_PERIOD)?;
        let entry = event.candle.close;

        // Previous bar's low/high for the lower-low / higher-high
        // capitulation confirmation.
        let len = history.len();
        let prev_candle = &history[len - 2];
        let curr_candle = &history[len - 1];

        let sl_offset = entry * SL_PCT;

        // Long setup: oversold extreme + capitulation candle
        if entry < lower && rsi < RSI_LONG_THRESHOLD && curr_candle.low < prev_candle.low {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                entry_price: entry,
                stop_loss: entry - sl_offset,
                // Take profit is dynamic (SMA20 mean-reach via on_open_positions);
                // park the fixed TP far away so the SL/TP monitor never trips it.
                take_profit: entry * dec!(1000),
                confidence: 0.65,
                timestamp: event.timestamp,
                allocation_pct: ALLOCATION_PCT,
                max_hold_until: Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS)),
                order_type: OrderType::Market,
            });
        }

        // Short setup: overbought extreme + capitulation candle
        if entry > upper && rsi > RSI_SHORT_THRESHOLD && curr_candle.high > prev_candle.high {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                entry_price: entry,
                stop_loss: entry + sl_offset,
                take_profit: entry / dec!(1000),
                confidence: 0.65,
                timestamp: event.timestamp,
                allocation_pct: ALLOCATION_PCT,
                max_hold_until: Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS)),
                order_type: OrderType::Market,
            });
        }

        None
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {
        // Mean-reversion ignores macro context.
    }

    async fn warmup(&mut self, events: &[PriceEvent]) {
        for event in events {
            if event.exchange != Exchange::BitflyerCfd {
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
        if event.exchange != Exchange::BitflyerCfd {
            return Vec::new();
        }
        let key = event.pair.0.clone();
        let Some(history) = self.history.get(&key) else {
            return Vec::new();
        };
        if history.is_empty() {
            return Vec::new();
        }

        let closes = Self::closes(history);
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
            // Mean-reversion target reached: long that retraced up to
            // SMA20 or short that retraced down to SMA20.
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
mod tests {
    use super::*;
    use auto_trader_core::types::{Candle, Exchange, Pair, Trade, TradeMode, TradeStatus};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_event(pair: &str, close: Decimal, high: Decimal, low: Decimal) -> PriceEvent {
        PriceEvent {
            pair: Pair::new(pair),
            exchange: Exchange::BitflyerCfd,
            timestamp: Utc::now(),
            candle: Candle {
                pair: Pair::new(pair),
                exchange: Exchange::BitflyerCfd,
                timeframe: "M5".to_string(),
                open: close,
                high,
                low,
                close,
                volume: Some(0),
                timestamp: Utc::now(),
            },
            indicators: HashMap::new(),
        }
    }

    fn make_position(strategy: &str, pair: &str, direction: Direction, entry: Decimal) -> Position {
        Position {
            trade: Trade {
                id: Uuid::new_v4(),
                strategy_name: strategy.to_string(),
                pair: Pair::new(pair),
                exchange: Exchange::BitflyerCfd,
                direction,
                entry_price: entry,
                exit_price: None,
                stop_loss: dec!(0),
                take_profit: dec!(0),
                quantity: Some(dec!(0.001)),
                leverage: dec!(2),
                fees: dec!(0),
                paper_account_id: None,
                entry_at: Utc::now(),
                exit_at: None,
                pnl_pips: None,
                pnl_amount: None,
                exit_reason: None,
                mode: TradeMode::Paper,
                status: TradeStatus::Open,
                max_hold_until: None,
                child_order_acceptance_id: None,
                child_order_id: None,
            },
        }
    }

    #[tokio::test]
    async fn no_signal_until_history_warmed() {
        let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let e = make_event("FX_BTC_JPY", dec!(10000000), dec!(10010000), dec!(9990000));
        assert!(s.on_price(&e).await.is_none());
    }

    #[tokio::test]
    async fn long_signal_at_oversold_extreme_with_capitulation() {
        let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        // Warm up: 30 candles around 10M JPY
        for i in 0..30 {
            let _ = s
                .on_price(&make_event(
                    "FX_BTC_JPY",
                    dec!(10000000) + Decimal::from(i * 100),
                    dec!(10005000) + Decimal::from(i * 100),
                    dec!(9995000) + Decimal::from(i * 100),
                ))
                .await;
        }
        // Sharp drop to push close below lower band, RSI < 25, lower-low
        let crash = make_event(
            "FX_BTC_JPY",
            dec!(9000000),
            dec!(9050000),
            dec!(8990000), // lower than previous lows
        );
        let signal = s.on_price(&crash).await;
        assert!(
            signal.is_some(),
            "expected long mean-revert signal at extreme"
        );
        let sig = signal.unwrap();
        assert_eq!(sig.direction, Direction::Long);
        // SL must be inside the 2% cap
        let cap = sig.entry_price * dec!(0.02);
        assert!(sig.entry_price - sig.stop_loss <= cap + dec!(0.001));
        // 24h fail-safe must be set
        assert!(sig.max_hold_until.is_some());
    }

    #[tokio::test]
    async fn open_positions_close_at_mean() {
        let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        // Warm history around 10M
        for _ in 0..30 {
            let _ = s
                .on_price(&make_event(
                    "FX_BTC_JPY",
                    dec!(10000000),
                    dec!(10005000),
                    dec!(9995000),
                ))
                .await;
        }
        // Long position bought at 9.5M
        let pos = make_position("bb", "FX_BTC_JPY", Direction::Long, dec!(9500000));
        // Mark price now back at 10M (≥ middle SMA20 = 10M) → exit
        let event = make_event("FX_BTC_JPY", dec!(10000000), dec!(10005000), dec!(9995000));
        let exits = s
            .on_open_positions(std::slice::from_ref(&pos), &event)
            .await;
        assert_eq!(exits.len(), 1, "expected 1 mean-reached exit");
        assert_eq!(exits[0].trade_id, pos.trade.id);
        assert_eq!(exits[0].reason, StrategyExitReason::MeanReached);
        assert_eq!(exits[0].close_price, dec!(10000000));
    }

    #[tokio::test]
    async fn open_positions_ignore_other_strategies() {
        let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        for _ in 0..30 {
            let _ = s
                .on_price(&make_event(
                    "FX_BTC_JPY",
                    dec!(10000000),
                    dec!(10005000),
                    dec!(9995000),
                ))
                .await;
        }
        let pos = make_position("not_bb", "FX_BTC_JPY", Direction::Long, dec!(9500000));
        let event = make_event("FX_BTC_JPY", dec!(10000000), dec!(10005000), dec!(9995000));
        let exits = s.on_open_positions(&[pos], &event).await;
        assert!(exits.is_empty());
    }
}
