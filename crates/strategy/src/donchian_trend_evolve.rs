//! Parameterizable Donchian Trend strategy (`donchian_trend_evolve_v1`).
//!
//! Mirrors [`super::donchian_trend::DonchianTrendV1`] in logic but reads
//! tunable channel parameters from a JSON params blob at construction time.
//! The params are loaded from the `strategy_params` DB table at startup and
//! refreshed by the weekly evolution batch.
//!
//! Default values (used when a key is absent) are identical to the baseline
//! `donchian_trend_v1` constants so a fresh deployment without DB params
//! produces equivalent behaviour.
//!
//! Stop-loss and position sizing are ATR-based (same as `donchian_trend_v1`):
//! hardcoded multiplier 3.0 and cap 5% for the SL, allocation 100% (full-bet
//! within PositionSizer no-liquidation constraint). These are not exposed as
//! evolvable params because changing them independently of the channel
//! parameters creates hard-to-interpret interactions.

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{
    ExitSignal, MacroUpdate, Strategy, StrategyExitReason, has_reached_one_r,
};
use auto_trader_core::types::{Candle, Direction, Pair, Position, Signal};
use auto_trader_market::indicators;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const HISTORY_LEN: usize = 200;
const ATR_PERIOD: usize = 14;
/// ATR multiplier for stop-loss — same as baseline donchian_trend_v1.
const ATR_MULT: Decimal = dec!(3.0);
/// Maximum stop-loss fraction — same as baseline.
const SL_CAP: Decimal = dec!(0.05);
// Allocation is always 100% — PositionSizer enforces no-liquidation constraint.
/// Maximum allocation per trade.
const ALLOCATION_CAP: Decimal = dec!(1.00);
/// This strategy uses 1H candles, same as baseline donchian_trend_v1.
const TIMEFRAME: &str = "H1";

pub struct DonchianTrendEvolveV1 {
    name: String,
    pairs: Vec<Pair>,
    entry_channel: usize,
    exit_channel: usize,
    atr_baseline_bars: usize,
    history: HashMap<String, VecDeque<Candle>>,
}

impl DonchianTrendEvolveV1 {
    /// Construct with params loaded from the `strategy_params` DB table.
    /// Falls back to the baseline `donchian_trend_v1` defaults for any
    /// missing key.
    ///
    /// Clamped to safe ranges so a bad LLM proposal that slipped past
    /// weekly_batch validation can't produce dangerous signals.
    pub fn new(name: String, pairs: Vec<Pair>, params: serde_json::Value) -> Self {
        let entry_channel = (params["entry_channel"].as_u64().unwrap_or(20) as usize).clamp(10, 30);
        let exit_channel = (params["exit_channel"].as_u64().unwrap_or(10) as usize).clamp(5, 15);
        let atr_baseline_bars =
            (params["atr_baseline_bars"].as_u64().unwrap_or(20) as usize).clamp(20, 100);
        Self {
            name,
            pairs,
            entry_channel,
            exit_channel,
            atr_baseline_bars,
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

    /// Average ATR over `atr_baseline_bars` bars prior to the current bar.
    /// Mirrors the baseline strategy's logic, parameterized by `self.atr_baseline_bars`.
    fn baseline_atr(&self, history: &VecDeque<Candle>) -> Option<Decimal> {
        if history.len() < self.atr_baseline_bars + ATR_PERIOD + 2 {
            return None;
        }
        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let closes = Self::closes(history);
        let latest_prior = history.len() - 2;
        let start = latest_prior + 1 - self.atr_baseline_bars;
        let mut sum = Decimal::ZERO;
        let mut count = 0u32;
        for end in start..=latest_prior {
            if end < ATR_PERIOD + 1 {
                continue;
            }
            if let Some(v) =
                indicators::atr(&highs[..=end], &lows[..=end], &closes[..=end], ATR_PERIOD)
            {
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
impl Strategy for DonchianTrendEvolveV1 {
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

        // Need enough history for the entry channel + ATR + baseline
        if history.len() < self.entry_channel + self.atr_baseline_bars + ATR_PERIOD + 1 {
            return None;
        }

        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let closes = Self::closes(history);

        // Entry channel: exclude the current bar for a clean breakout check
        let (channel_low, channel_high) =
            indicators::donchian_channel(&highs, &lows, self.entry_channel, false)?;

        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;
        let baseline = self.baseline_atr(history)?;
        if atr <= baseline {
            // Volatility too tame — likely a false breakout. Skip.
            return None;
        }

        let entry = event.candle.close;
        // ATR-based stop-loss, capped at SL_CAP.
        let stop_loss_pct = (atr * ATR_MULT / entry).min(SL_CAP);
        if stop_loss_pct <= Decimal::ZERO {
            return None; // ATR=0, no volatility to trade
        }
        let allocation_pct = ALLOCATION_CAP;

        if entry > channel_high {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                stop_loss_pct,
                // Trailing exit handled in on_open_positions (Turtle has no fixed TP).
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
        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let Some((exit_low, exit_high)) =
            indicators::donchian_channel(&highs, &lows, self.exit_channel, false)
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

            // 1R minimum for this trailing exit: don't activate until
            // unrealized profit >= SL distance. Only affects this exit path;
            // other mechanisms may still close before 1R.
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
                timeframe: "H1".to_string(),
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

    fn default_params() -> serde_json::Value {
        serde_json::json!({
            "entry_channel": 20,
            "exit_channel": 10,
            "atr_baseline_bars": 50
        })
    }

    #[test]
    fn constructor_parses_json_params() {
        let params = serde_json::json!({
            "entry_channel": 18,
            "exit_channel": 8,
            "atr_baseline_bars": 30
        });
        let s =
            DonchianTrendEvolveV1::new("test".to_string(), vec![Pair::new("FX_BTC_JPY")], params);
        assert_eq!(s.entry_channel, 18);
        assert_eq!(s.exit_channel, 8);
        assert_eq!(s.atr_baseline_bars, 30);
    }

    #[test]
    fn constructor_uses_defaults_for_missing_keys() {
        let s = DonchianTrendEvolveV1::new(
            "test".to_string(),
            vec![Pair::new("FX_BTC_JPY")],
            serde_json::json!({}),
        );
        assert_eq!(s.entry_channel, 20);
        assert_eq!(s.exit_channel, 10);
        assert_eq!(s.atr_baseline_bars, 20);
    }

    /// Legacy sl_pct / allocation_pct keys in the JSON must be silently ignored
    /// (they were removed in favour of ATR-based calculation).
    #[test]
    fn constructor_ignores_legacy_sl_allocation_params() {
        let params = serde_json::json!({
            "entry_channel": 20,
            "exit_channel": 10,
            "sl_pct": 0.04,       // formerly configurable, now ignored
            "allocation_pct": 0.8, // formerly configurable, now ignored
            "atr_baseline_bars": 50
        });
        let s =
            DonchianTrendEvolveV1::new("test".to_string(), vec![Pair::new("FX_BTC_JPY")], params);
        // Only channel params should be parsed; SL and allocation are ATR-derived at runtime.
        // atr_baseline_bars=50 is explicitly set in JSON and within clamp range [20,100].
        assert_eq!(s.entry_channel, 20);
        assert_eq!(s.exit_channel, 10);
        assert_eq!(s.atr_baseline_bars, 50);
    }

    #[tokio::test]
    async fn no_signal_with_insufficient_history() {
        let mut s = DonchianTrendEvolveV1::new(
            "dte".to_string(),
            vec![Pair::new("FX_BTC_JPY")],
            default_params(),
        );
        let e = make_event("FX_BTC_JPY", dec!(10000000), dec!(10005000), dec!(9995000));
        assert!(s.on_price(&e).await.is_none());
    }

    #[tokio::test]
    async fn long_breakout_with_atr_based_sl() {
        let params = serde_json::json!({
            "entry_channel": 20,
            "exit_channel": 10,
            "atr_baseline_bars": 50
        });
        let mut s =
            DonchianTrendEvolveV1::new("dte".to_string(), vec![Pair::new("FX_BTC_JPY")], params);
        // 100 calm bars
        for i in 0..100 {
            let drift = Decimal::from(i % 5) * dec!(1000);
            let _ = s
                .on_price(&make_event(
                    "FX_BTC_JPY",
                    dec!(10000000) + drift,
                    dec!(10010000) + drift,
                    dec!(9990000) + drift,
                ))
                .await;
        }
        // Breakout bar
        let breakout = make_event("FX_BTC_JPY", dec!(11000000), dec!(11200000), dec!(10800000));
        let signal = s.on_price(&breakout).await;
        assert!(signal.is_some(), "expected long breakout signal");
        let sig = signal.unwrap();
        assert_eq!(sig.direction, Direction::Long);
        // ATR-based SL: positive and at most SL_CAP (5%).
        assert!(
            sig.stop_loss_pct > Decimal::ZERO && sig.stop_loss_pct <= dec!(0.05),
            "ATR-based SL must be in (0, SL_CAP=5%], got {}",
            sig.stop_loss_pct
        );
        // Risk-linked allocation: at most ALLOCATION_CAP (100%).
        assert!(sig.allocation_pct > Decimal::ZERO);
        assert!(sig.allocation_pct <= dec!(1.00));
        // Dynamic exit strategy → TP is None
        assert!(sig.take_profit_pct.is_none());
    }

    #[tokio::test]
    async fn position_closes_on_trailing_channel() {
        let mut s = DonchianTrendEvolveV1::new(
            "dte".to_string(),
            vec![Pair::new("FX_BTC_JPY")],
            default_params(),
        );
        for _ in 0..100 {
            let _ = s
                .on_price(&make_event(
                    "FX_BTC_JPY",
                    dec!(11000000),
                    dec!(11050000),
                    dec!(10950000),
                ))
                .await;
        }
        // Entry 10000000, SL 9800000 → sl_distance=200000.
        // Drop close = 10500000 → unrealized=500000 >= 200000 (1R passes).
        // Exit channel low (10 bars of lows=10950000) = 10950000.
        // 10500000 < 10950000 → trailing break fires.
        let pos = Position {
            trade: Trade {
                id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                strategy_name: "dte".to_string(),
                pair: Pair::new("FX_BTC_JPY"),
                exchange: Exchange::BitflyerCfd,
                direction: Direction::Long,
                entry_price: dec!(10000000),
                exit_price: None,
                stop_loss: dec!(9800000),
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
        };
        let drop_event = make_event("FX_BTC_JPY", dec!(10500000), dec!(10550000), dec!(10450000));
        let _ = s.on_price(&drop_event).await;
        let exits = s
            .on_open_positions(std::slice::from_ref(&pos), &drop_event)
            .await;
        assert_eq!(exits.len(), 1);
        assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);
    }

    #[tokio::test]
    async fn position_short_exits_on_trailing_after_1r() {
        let mut s = DonchianTrendEvolveV1::new(
            "dte".to_string(),
            vec![Pair::new("FX_BTC_JPY")],
            default_params(),
        );
        // 100 bars at 11M. Exit channel high (10 bars of highs=11050000) = 11050000.
        for _ in 0..100 {
            let _ = s
                .on_price(&make_event(
                    "FX_BTC_JPY",
                    dec!(11000000),
                    dec!(11050000),
                    dec!(10950000),
                ))
                .await;
        }
        // Short entry at 11300000, SL at 11500000 → sl_distance=200000.
        // Spike close at 11100000: unrealized=11300000-11100000=200000 >= 200000 (1R boundary, passes).
        // 11100000 > exit_high=11050000 → trailing break for Short → exit.
        let pos = Position {
            trade: Trade {
                id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                strategy_name: "dte".to_string(),
                pair: Pair::new("FX_BTC_JPY"),
                exchange: Exchange::BitflyerCfd,
                direction: Direction::Short,
                entry_price: dec!(11300000),
                exit_price: None,
                stop_loss: dec!(11500000),
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
        };
        let spike = make_event("FX_BTC_JPY", dec!(11100000), dec!(11150000), dec!(11050000));
        let _ = s.on_price(&spike).await;
        let exits = s
            .on_open_positions(std::slice::from_ref(&pos), &spike)
            .await;
        assert_eq!(exits.len(), 1, "expected trailing channel exit for Short");
        assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);
        assert_eq!(exits[0].close_price, dec!(11100000));
    }

    /// 1R guard: if unrealized profit < SL distance, the trailing channel
    /// exit must NOT fire even when close is below the exit-channel low.
    #[tokio::test]
    async fn position_no_exit_when_1r_not_reached() {
        let mut s = DonchianTrendEvolveV1::new(
            "dte".to_string(),
            vec![Pair::new("FX_BTC_JPY")],
            default_params(),
        );
        // 100 bars at 11M; exit channel low = 10950000.
        for _ in 0..100 {
            let _ = s
                .on_price(&make_event(
                    "FX_BTC_JPY",
                    dec!(11000000),
                    dec!(11050000),
                    dec!(10950000),
                ))
                .await;
        }
        // Entry 10800000, SL 10600000 → sl_distance=200000.
        // Close 10900000 → unrealized = 10900000 - 10800000 = 100000 (0 < 100000 < 200000).
        // 10900000 < exit_low=10950000, so trailing would fire without guard.
        // 1R guard: 100000 < 200000 → no exit.
        let pos = Position {
            trade: Trade {
                id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                strategy_name: "dte".to_string(),
                pair: Pair::new("FX_BTC_JPY"),
                exchange: Exchange::BitflyerCfd,
                direction: Direction::Long,
                entry_price: dec!(10800000),
                exit_price: None,
                stop_loss: dec!(10600000),
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
        };
        let drop_event = make_event("FX_BTC_JPY", dec!(10900000), dec!(10950000), dec!(10850000));
        let _ = s.on_price(&drop_event).await;
        let exits = s
            .on_open_positions(std::slice::from_ref(&pos), &drop_event)
            .await;
        assert!(
            exits.is_empty(),
            "1R not reached → no trailing exit, got {} exits",
            exits.len()
        );
    }
}
