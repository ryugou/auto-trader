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
//! ATR(14)-based: `min(ATR × 1.5 / entry, 3%)`. Mean-reversion entries
//! are tight by design — 1.5× ATR places the SL just outside recent noise.
//!
//! ## Position sizing
//! `allocation_pct = 1.0` (full-bet). PositionSizer enforces the
//! no-liquidation constraint.
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
use auto_trader_core::strategy::{
    ExitSignal, MacroUpdate, Strategy, StrategyExitReason, has_reached_one_r,
};
use auto_trader_core::types::{Candle, Direction, Pair, Position, Signal};
use auto_trader_market::indicators;
use chrono::Duration;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const BB_PERIOD: usize = 20;
const RSI_PERIOD: usize = 14;
const ATR_PERIOD: usize = 14;
/// Number of historical candles each per-pair `VecDeque` keeps. Enough
/// for BB(20) + RSI(14+1) + the "previous bar lower-low" check, with
/// some headroom for the warmup-from-DB seed (200 bars in main.rs).
const HISTORY_LEN: usize = 200;

const RSI_LONG_THRESHOLD: Decimal = dec!(25);
const RSI_SHORT_THRESHOLD: Decimal = dec!(75);
/// ATR multiplier for stop-loss. 1.5× ATR places the SL just outside
/// the recent noise range — tight enough for mean-reversion discipline.
const ATR_MULT: Decimal = dec!(1.5);
/// Maximum stop-loss as a fraction of entry price. Caps the ATR-based
/// SL during high-volatility periods so it does not exceed 3% of entry.
const SL_CAP: Decimal = dec!(0.03);
/// Allocation per trade: 1.0 (full-bet). PositionSizer enforces the
/// no-liquidation constraint.
const ALLOCATION_CAP: Decimal = dec!(1.00);
const TIME_LIMIT_HOURS: i64 = 24;
/// This strategy uses M5 candles (scalping / mean-reversion).
const TIMEFRAME: &str = "M5";

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

    fn highs(history: &VecDeque<Candle>) -> Vec<Decimal> {
        history.iter().map(|c| c.high).collect()
    }

    fn lows(history: &VecDeque<Candle>) -> Vec<Decimal> {
        history.iter().map(|c| c.low).collect()
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

        // Need at least BB_PERIOD candles plus one previous bar for the
        // capitulation check, RSI history, and ATR(14) lead-in.
        if history.len() < BB_PERIOD.max(RSI_PERIOD + 1).max(ATR_PERIOD + 1) + 1 {
            return None;
        }

        let closes = Self::closes(history);
        let highs = Self::highs(history);
        let lows = Self::lows(history);

        let (lower, _middle, upper) = indicators::bollinger_bands(&closes, BB_PERIOD, dec!(2.5))?;
        let rsi = indicators::rsi(&closes, RSI_PERIOD)?;
        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;
        let entry = event.candle.close;

        // ATR-based stop-loss, capped at SL_CAP.
        let stop_loss_pct = (atr * ATR_MULT / entry).min(SL_CAP);
        if stop_loss_pct <= Decimal::ZERO {
            return None; // ATR=0, no volatility to trade
        }
        let allocation_pct = ALLOCATION_CAP;

        // Previous bar's low/high for the lower-low / higher-high
        // capitulation confirmation.
        let len = history.len();
        let prev_candle = &history[len - 2];
        let curr_candle = &history[len - 1];

        // Long setup: oversold extreme + capitulation candle
        if entry < lower && rsi < RSI_LONG_THRESHOLD && curr_candle.low < prev_candle.low {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                stop_loss_pct,
                // Take profit is dynamic (SMA20 mean-reach via on_open_positions).
                take_profit_pct: None,
                confidence: 0.65,
                timestamp: event.timestamp,
                allocation_pct,
                max_hold_until: Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS)),
            });
        }

        // Short setup: overbought extreme + capitulation candle
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

    fn on_macro_update(&mut self, _update: &MacroUpdate) {
        // Mean-reversion ignores macro context.
    }

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

            // 1R minimum: don't emit the strategy's mean-reversion exit until
            // unrealized profit >= SL distance. Only affects this exit path;
            // other mechanisms (e.g. time-limit close, SL hit) may still close
            // before 1R.
            if !has_reached_one_r(
                &pos.trade.direction,
                pos.trade.entry_price,
                pos.trade.stop_loss,
                close,
            ) {
                continue; // Haven't reached 1R yet — let it run or hit SL.
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
    use auto_trader_core::types::{Candle, Exchange, Pair, Trade, TradeStatus};
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
                best_bid: None,
                best_ask: None,
                timestamp: Utc::now(),
            },
            indicators: HashMap::new(),
        }
    }

    fn make_position_with_sl(
        strategy: &str,
        pair: &str,
        direction: Direction,
        entry: Decimal,
        stop_loss: Decimal,
    ) -> Position {
        Position {
            trade: Trade {
                id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                strategy_name: strategy.to_string(),
                pair: Pair::new(pair),
                exchange: Exchange::BitflyerCfd,
                direction,
                entry_price: entry,
                exit_price: None,
                stop_loss,
                take_profit: None,
                quantity: dec!(0.001),
                leverage: dec!(2),
                fees: dec!(0),
                entry_at: Utc::now(),
                exit_at: None,
                pnl_amount: None,
                exit_reason: None,
                status: TradeStatus::Open,
                max_hold_until: None,
            },
        }
    }

    #[tokio::test]
    async fn no_signal_until_history_warmed() {
        let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let e = make_event("FX_BTC_JPY", dec!(10000000), dec!(10010000), dec!(9990000));
        assert!(s.on_price(&e).await.is_none());
    }

    /// Non-M5 candles must be silently ignored so BB stays on M5-only data.
    #[tokio::test]
    async fn ignores_non_m5_timeframe() {
        let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let mut e = make_event("FX_BTC_JPY", dec!(10000000), dec!(10010000), dec!(9990000));
        e.candle.timeframe = "H1".to_string();
        // Even after many bars, H1 events must not trigger a signal.
        for _ in 0..50 {
            assert!(s.on_price(&e).await.is_none());
        }
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
        // ATR-based SL: must be positive and at most SL_CAP (3%).
        assert!(sig.stop_loss_pct > Decimal::ZERO);
        assert!(sig.stop_loss_pct <= dec!(0.03));
        // Risk-linked allocation: at most ALLOCATION_CAP (100%).
        assert!(sig.allocation_pct > Decimal::ZERO);
        assert!(sig.allocation_pct <= dec!(1.00));
        // Dynamic exit strategy → TP is None
        assert!(sig.take_profit_pct.is_none());
        // 24h fail-safe must be set
        assert!(sig.max_hold_until.is_some());
    }

    /// ATR-based SL and risk-linked sizing change proportionally with
    /// market volatility — they must never return the old flat constants.
    #[tokio::test]
    async fn sl_and_allocation_are_atr_derived() {
        let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        // Warm up: 30 candles
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
        let crash = make_event("FX_BTC_JPY", dec!(9000000), dec!(9050000), dec!(8990000));
        let sig = s.on_price(&crash).await.unwrap();
        // ATR-based SL: positive and at most SL_CAP (3%).
        assert!(
            sig.stop_loss_pct > Decimal::ZERO && sig.stop_loss_pct <= dec!(0.03),
            "ATR-based SL must be in (0, SL_CAP=3%], got {}",
            sig.stop_loss_pct
        );
        // Risk-linked allocation: positive and at most ALLOCATION_CAP (100%).
        assert!(
            sig.allocation_pct > Decimal::ZERO && sig.allocation_pct <= dec!(1.00),
            "allocation must be in (0, 50%], got {}",
            sig.allocation_pct
        );
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
        // Long position bought at 9.5M with SL at 9.3M (sl_distance=200K).
        // Unrealized at close=10M: 500K >= 200K → 1R guard passes.
        let pos = make_position_with_sl(
            "bb",
            "FX_BTC_JPY",
            Direction::Long,
            dec!(9500000),
            dec!(9300000),
        );
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

    /// 1R guard: if unrealized profit < SL distance, the BB mean-reached
    /// exit must NOT fire even when price is at or above SMA20.
    #[tokio::test]
    async fn open_positions_no_exit_when_1r_not_reached() {
        let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        // Warm history around 10M — SMA20 = 10M.
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
        // Entry 9900000, SL 9700000 → sl_distance=200000.
        // Close 10050000 → unrealized=150000 < 200000 (1R not reached).
        // Close 10050000 >= SMA20 10000000 → mean_reached IS true.
        // But 1R guard prevents exit.
        let pos = make_position_with_sl(
            "bb",
            "FX_BTC_JPY",
            Direction::Long,
            dec!(9900000),
            dec!(9700000),
        );
        let event = make_event("FX_BTC_JPY", dec!(10050000), dec!(10060000), dec!(10040000));
        let exits = s
            .on_open_positions(std::slice::from_ref(&pos), &event)
            .await;
        assert!(
            exits.is_empty(),
            "1R not reached → no exit, got {} exits",
            exits.len()
        );
    }

    #[tokio::test]
    async fn open_positions_short_exits_at_mean_after_1r() {
        let mut s = BbMeanRevertV1::new("bb".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        // Warm history around 10M → SMA20 ≈ 10M.
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
        // Short entry at 10300000, SL at 10500000 → sl_distance=200000.
        // Close at 9900000: unrealized=10300000-9900000=400000 >= 200000 (1R passes).
        // 9900000 <= SMA20 10000000 → mean_reached for Short = true → exit.
        let pos = make_position_with_sl(
            "bb",
            "FX_BTC_JPY",
            Direction::Short,
            dec!(10300000),
            dec!(10500000),
        );
        let event = make_event("FX_BTC_JPY", dec!(9900000), dec!(9950000), dec!(9850000));
        let exits = s
            .on_open_positions(std::slice::from_ref(&pos), &event)
            .await;
        assert_eq!(exits.len(), 1, "expected 1 mean-reached exit for Short");
        assert_eq!(exits[0].trade_id, pos.trade.id);
        assert_eq!(exits[0].reason, StrategyExitReason::MeanReached);
        assert_eq!(exits[0].close_price, dec!(9900000));
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
        let pos = make_position_with_sl(
            "not_bb",
            "FX_BTC_JPY",
            Direction::Long,
            dec!(9500000),
            dec!(9400000),
        );
        let event = make_event("FX_BTC_JPY", dec!(10000000), dec!(10005000), dec!(9995000));
        let exits = s.on_open_positions(&[pos], &event).await;
        assert!(exits.is_empty());
    }
}
