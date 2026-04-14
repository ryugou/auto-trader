//! 標準ブレイクアウト v1 (`donchian_trend_v1`).
//!
//! Turtle Traders System 1 adapted for FX_BTC_JPY (M5). The original
//! Turtle System enters on a 20-day breakout and exits on a 10-day
//! reverse breakout, intentionally avoiding fixed take-profit targets so
//! winners can run ([Original Turtle Trading Rules](https://oxfordstrat.com/coasdfASD32/uploads/2016/01/turtle-rules.pdf)).
//!
//! This implementation uses 20-bar / 10-bar Donchian channels on the M5
//! timeframe — proportionally short relative to "20 days" but appropriate
//! for the higher trade frequency of crypto.
//!
//! ## Entry rules
//! - **Long**: previous bar's close > prior 20-bar high AND ATR(14) >
//!   the rolling average ATR over the prior 50 bars (volatility filter
//!   to suppress low-energy false breakouts).
//! - **Short**: mirror — close < 20-bar low AND elevated ATR.
//!
//! ## Stop loss
//! Flat **3 % from entry price** (`SL_PCT`). Trend strategies want a
//! slightly wider safety net than mean-reversion ones because the entry
//! is on a momentum break and small retraces are normal. Sizing is
//! independent of the SL distance — see `allocation_pct` on Signal.
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
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy, StrategyExitReason};
use auto_trader_core::types::{Candle, Direction, Exchange, OrderType, Pair, Position, Signal};
use auto_trader_market::indicators;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const ENTRY_CHANNEL: usize = 20;
const EXIT_CHANNEL: usize = 10;
const ATR_PERIOD: usize = 14;
const ATR_BASELINE_BARS: usize = 50;
/// Stop-loss as a flat percentage of entry price. Trend strategies want
/// a slightly wider safety net than mean-reversion ones because the
/// entry is on a momentum break and short-term retraces are normal.
const SL_PCT: Decimal = dec!(0.03);
/// Capital allocation per trade. Trend trades are infrequent but
/// directionally strong, and this strategy has its own dedicated paper
/// account — leaving cash idle is pure waste. 100 % is safe at 2×
/// leverage + a 3 % SL: the SL fires before the maintenance-margin
/// threshold and the exchange can't auto-liquidate first.
const ALLOCATION_PCT: Decimal = dec!(1.00);
const HISTORY_LEN: usize = 200;

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
        if event.exchange != Exchange::BitflyerCfd {
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
        let sl_offset = entry * SL_PCT;

        if entry > channel_high {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                entry_price: entry,
                stop_loss: entry - sl_offset,
                // Trailing exit handled in on_open_positions; the fixed
                // TP is parked far away so the SL/TP monitor never fires
                // it (Turtle System has no fixed TP by design).
                take_profit: entry * dec!(1000),
                confidence: 0.6,
                timestamp: event.timestamp,
                allocation_pct: ALLOCATION_PCT,
                max_hold_until: None,
                order_type: OrderType::Market,
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
                allocation_pct: ALLOCATION_PCT,
                max_hold_until: None,
                order_type: OrderType::Market,
            });
        }
        None
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {}

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
    use auto_trader_core::types::{Candle, Pair, Trade, TradeMode, TradeStatus};
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

    fn make_position(strategy: &str, direction: Direction, entry: Decimal) -> Position {
        Position {
            trade: Trade {
                id: Uuid::new_v4(),
                strategy_name: strategy.to_string(),
                pair: Pair::new("FX_BTC_JPY"),
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
            },
        }
    }

    #[tokio::test]
    async fn no_signal_with_insufficient_history() {
        let mut s = DonchianTrendV1::new("dt".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let e = make_event("FX_BTC_JPY", dec!(10000000), dec!(10005000), dec!(9995000));
        assert!(s.on_price(&e).await.is_none());
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
        let cap = sig.entry_price * dec!(0.03);
        assert!(sig.entry_price - sig.stop_loss <= cap + dec!(0.001));
        // Turtle has NO fixed TP — it's parked extremely far away
        assert!(sig.take_profit > sig.entry_price * dec!(10));
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
        // Long position from earlier
        let pos = make_position("dt", Direction::Long, dec!(11000000));
        // Now price drops below the 10-bar low.
        let drop = make_event("FX_BTC_JPY", dec!(10500000), dec!(10550000), dec!(10450000));
        // First push the drop into history
        let _ = s.on_price(&drop).await;
        let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
        assert_eq!(exits.len(), 1, "expected trailing channel exit");
        assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);
        assert_eq!(exits[0].close_price, dec!(10500000));
    }
}
