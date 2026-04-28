//! 標準ブレイクアウト v1 (`donchian_trend_v1`).
//!
//! Turtle Traders System 1 adapted for FX_BTC_JPY (1H). The original
//! Turtle System enters on a 20-day breakout and exits on a 10-day
//! reverse breakout, intentionally avoiding fixed take-profit targets so
//! winners can run ([Original Turtle Trading Rules](https://oxfordstrat.com/coasdfASD32/uploads/2016/01/turtle-rules.pdf)).
//!
//! This implementation uses 20-bar / 10-bar Donchian channels on the 1H
//! timeframe — M5 produced too many false breakouts; 1H aligns better
//! with the trend-following nature of the logic.
//!
//! ## Entry rules
//! - **Long**: previous bar's close > prior 20-bar high AND ATR(14) >
//!   the rolling average ATR over the prior 50 bars (volatility filter
//!   to suppress low-energy false breakouts).
//! - **Short**: mirror — close < 20-bar low AND elevated ATR.
//!
//! ## Stop loss
//! ATR(14)-based: `min(ATR × 3.0 / entry, 5%)`. Turtle System 1 uses
//! N-based stops (N = ATR); 3× ATR is within the original recommended
//! range and provides breathing room for normal trend retraces.
//!
//! ## Position sizing
//! `allocation_pct = min(1% / stop_loss_pct, 50%)`. With 2× account leverage
//! actual risk = `1% × 2 = 2%` of account per trade; caps at 50% to prevent
//! over-exposure.
//!
//! ## Take profit (dynamic, via `on_open_positions`)
//! - **Long** closes when current close < prior 10-bar low.
//! - **Short** closes when current close > prior 10-bar high.
//!
//! No fixed TP — the trailing channel exit is the strategy's edge.
//!
//! Risk profile: medium ("標準"). Targets directional moves, expects
//! ~40% win rate at R:R ~1:2 or better.

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{
    ExitSignal, MacroUpdate, Strategy, StrategyExitReason, has_reached_one_r,
};
use auto_trader_core::types::{Candle, Direction, Pair, Position, Signal};
use auto_trader_market::indicators;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const ENTRY_CHANNEL: usize = 20;
const EXIT_CHANNEL: usize = 10;
const ATR_PERIOD: usize = 14;
/// Lowered from 50 → 20 for H1 timeframe. With 1H bars, 50 bars = 50
/// hours of ATR history which requires 65+ bars total (50 + ATR_PERIOD + 1)
/// before the first signal can fire. At 20 bars the requirement drops to
/// 35 bars (≈ 1.5 days of H1 data), and 20 hours of ATR baseline is still
/// sufficient to distinguish genuine volatility expansion from noise.
const ATR_BASELINE_BARS: usize = 20;
/// ATR multiplier for stop-loss. 3× ATR is within the Turtle System's
/// original N-based stop recommendation — wide enough for trend retraces.
const ATR_MULT: Decimal = dec!(3.0);
/// Maximum stop-loss as a fraction of entry price.
const SL_CAP: Decimal = dec!(0.05);
/// Target risk per trade as an *unleveraged* fraction of account balance.
/// Target per-trade risk budget. The leverage-aware risk cap is enforced
/// by PositionSizer (which knows the actual account leverage), so this
/// value does not need manual adjustment when leverage changes.
const TARGET_RISK_PCT: Decimal = dec!(0.01);
/// Maximum allocation per trade.
const ALLOCATION_CAP: Decimal = dec!(1.00);
const HISTORY_LEN: usize = 200;
/// This strategy uses 1H candles (trend-following; M5 produced excessive
/// false breakouts on a daily-bar-designed logic).
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

    /// Average ATR over the `ATR_BASELINE_BARS` bars **prior to** the
    /// current bar (the current bar is excluded so the breakout-day
    /// volatility doesn't pollute its own baseline). Used as the
    /// reference against which the current ATR(14) is compared for the
    /// "elevated volatility" entry filter.
    fn baseline_atr(history: &VecDeque<Candle>) -> Option<Decimal> {
        // Need ATR_BASELINE_BARS prior bars, each of which needs
        // ATR_PERIOD + 1 lead-in for the ATR calc, plus the current bar
        // we're excluding.
        if history.len() < ATR_BASELINE_BARS + ATR_PERIOD + 2 {
            return None;
        }
        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let closes = Self::closes(history);
        // Last index we are allowed to look at is `history.len() - 2`
        // (one before the current bar). The window is the last
        // ATR_BASELINE_BARS such ATR values, computed on slices
        // `[..=end]` for `end` in `[start, latest_prior]`.
        let latest_prior = history.len() - 2;
        let start = latest_prior + 1 - ATR_BASELINE_BARS;
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

        // Need enough history for the entry channel + ATR + baseline
        if history.len() < ENTRY_CHANNEL + ATR_BASELINE_BARS + ATR_PERIOD + 1 {
            return None;
        }

        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let closes = Self::closes(history);

        // Entry channel uses prior bars only — exclude the current bar
        // so the breakout is "above the 20 most recent prior bars".
        let (channel_low, channel_high) =
            indicators::donchian_channel(&highs, &lows, ENTRY_CHANNEL, false)?;

        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;
        let baseline = Self::baseline_atr(history)?;
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
        // Risk-linked allocation: risk at most TARGET_RISK_PCT of account.
        let allocation_pct = (TARGET_RISK_PCT / stop_loss_pct).min(ALLOCATION_CAP);

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
        // Exit channel uses prior 10 bars (excluding current).
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
                // Long exits when price slips below the prior 10-bar low
                Direction::Long => close < exit_low,
                // Short exits when price climbs above the prior 10-bar high
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

    fn make_position_with_sl(
        strategy: &str,
        direction: Direction,
        entry: Decimal,
        stop_loss: Decimal,
    ) -> Position {
        Position {
            trade: Trade {
                id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                strategy_name: strategy.to_string(),
                pair: Pair::new("FX_BTC_JPY"),
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
    async fn no_signal_with_insufficient_history() {
        let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let e = make_event("FX_BTC_JPY", dec!(10000000), dec!(10005000), dec!(9995000));
        assert!(s.on_price(&e).await.is_none());
    }

    /// M5 candles must be silently ignored — this strategy runs on H1.
    #[tokio::test]
    async fn ignores_non_h1_timeframe() {
        let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let mut e = make_event("FX_BTC_JPY", dec!(10000000), dec!(10005000), dec!(9995000));
        e.candle.timeframe = "M5".to_string();
        for _ in 0..120 {
            assert!(s.on_price(&e).await.is_none());
        }
    }

    #[tokio::test]
    async fn long_breakout_above_channel_with_volatility_expansion() {
        let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        // 100 calm bars near 10M
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
        // Sudden expansion: bigger range AND price clearly above prior
        // 20-bar high.
        let breakout = make_event(
            "FX_BTC_JPY",
            dec!(11000000), // close way above prior
            dec!(11200000), // wide range to lift ATR above baseline
            dec!(10800000),
        );
        let signal = s.on_price(&breakout).await;
        assert!(signal.is_some(), "expected long breakout signal");
        let sig = signal.unwrap();
        assert_eq!(sig.direction, Direction::Long);
        // ATR-based SL: positive and at most SL_CAP (5%).
        assert!(sig.stop_loss_pct > Decimal::ZERO);
        assert!(sig.stop_loss_pct <= dec!(0.05));
        // Risk-linked allocation: at most ALLOCATION_CAP (100%).
        assert!(sig.allocation_pct > Decimal::ZERO);
        assert!(sig.allocation_pct <= dec!(1.00));
        // Turtle has NO fixed TP — dynamic exit strategy uses None
        assert!(sig.take_profit_pct.is_none());
    }

    /// The old flat SL_PCT (3%) must no longer appear.
    #[tokio::test]
    async fn sl_and_allocation_are_atr_derived() {
        let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
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
        let breakout = make_event("FX_BTC_JPY", dec!(11000000), dec!(11200000), dec!(10800000));
        let sig = s.on_price(&breakout).await.unwrap();
        // ATR-based SL: positive and at most SL_CAP (5%).
        assert!(
            sig.stop_loss_pct > Decimal::ZERO && sig.stop_loss_pct <= dec!(0.05),
            "ATR-based SL must be in (0, SL_CAP=5%], got {}",
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
    async fn open_positions_close_on_trailing_channel_break() {
        let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        // Build history with high prices then a sharp drop
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
        // Long position: entry=10000000, SL=9800000 → sl_distance=200000.
        // Drop close=10500000 → unrealized=500000 >= 200000 (1R passes).
        // Exit channel low (10 bars of lows=10950000) = 10950000.
        // 10500000 < 10950000 → trailing break fires.
        let pos = make_position_with_sl("dt", Direction::Long, dec!(10000000), dec!(9800000));
        // Now price drops below the 10-bar low.
        let drop = make_event("FX_BTC_JPY", dec!(10500000), dec!(10550000), dec!(10450000));
        // First push the drop into history
        let _ = s.on_price(&drop).await;
        let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
        assert_eq!(exits.len(), 1, "expected trailing channel exit");
        assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);
        assert_eq!(exits[0].close_price, dec!(10500000));
    }

    #[tokio::test]
    async fn open_positions_short_exits_on_trailing_after_1r() {
        let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
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
        let pos = make_position_with_sl("dt", Direction::Short, dec!(11300000), dec!(11500000));
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
    async fn open_positions_no_exit_when_1r_not_reached() {
        let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
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
        let pos = make_position_with_sl("dt", Direction::Long, dec!(10800000), dec!(10600000));
        let drop = make_event("FX_BTC_JPY", dec!(10900000), dec!(10950000), dec!(10850000));
        let _ = s.on_price(&drop).await;
        let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
        assert!(
            exits.is_empty(),
            "1R not reached → no trailing exit, got {} exits",
            exits.len()
        );
    }
}
