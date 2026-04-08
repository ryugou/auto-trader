//! 攻めボラティリティ v1 (`squeeze_momentum_v1`).
//!
//! TTM Squeeze (John Carter) adapted for FX_BTC_JPY (M5). The textbook
//! TTM Squeeze fires when Bollinger Bands compress *inside* the Keltner
//! Channels for a sustained period and then re-expand outside, signaling
//! a volatility breakout. The bias direction is taken from the momentum
//! histogram (close vs SMA) — we use a simplified `close - SMA(20)` as
//! the momentum proxy ([TrendSpider TTM Squeeze guide](https://trendspider.com/learning-center/introduction-to-ttm-squeeze/),
//! [EBC Financial overview](https://www.ebc.com/forex/top-ways-to-master-the-ttm-squeeze-trading-strategy)).
//!
//! ## Entry rules
//! - **Squeeze condition**: BB(20, 2σ) is fully inside KC(20, 1.5×ATR)
//!   for at least `SQUEEZE_BARS` consecutive bars.
//! - **Squeeze fires**: BB exits the Keltner Channel on the current bar.
//! - **Long**: squeeze fires AND momentum (`close - SMA20`) is positive
//!   AND momentum increased vs. the prior bar.
//! - **Short**: squeeze fires AND momentum is negative AND decreased.
//!
//! ## Stop loss
//! Flat **4 % from entry price** (`SL_PCT`). Squeeze releases can
//! whipsaw — give the trade enough room to survive a single bad bar.
//! Sizing is independent of the SL distance — see `allocation_pct` on
//! Signal.
//!
//! ## Take profit (dynamic, via `on_open_positions`)
//! - **Long** closes when current close < EMA(21).
//! - **Short** closes when current close > EMA(21).
//! - 48-hour fail-safe via `max_hold_until`.
//!
//! No fixed TP — the EMA-based trailing stop captures as much of the
//! breakout as possible.
//!
//! Risk profile: high ("攻め"). Targets sudden volatility expansion (news,
//! crashes, breakouts), expects ~30% win rate but R:R 1:3+ on winners.

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy, StrategyExitReason};
use auto_trader_core::types::{Candle, Direction, Exchange, Pair, Position, Signal};
use auto_trader_market::indicators;
use chrono::Duration;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const BB_PERIOD: usize = 20;
const BB_STDDEV: Decimal = dec!(2);
const KC_PERIOD: usize = 20;
const KC_ATR_MULT: Decimal = dec!(1.5);
const ATR_PERIOD: usize = 14;
const EMA_TRAIL_PERIOD: usize = 21;
const SQUEEZE_BARS: usize = 6;
/// Bars looked back for the historical-needed minimum (kept for the
/// `len < needed + 2` guard even though SL is now flat-percentage).
const SWING_LOOKBACK: usize = 5;
/// Stop-loss as a flat percentage of entry price. Squeeze releases can
/// whipsaw — give the trade enough room to survive a single bad bar.
const SL_PCT: Decimal = dec!(0.04);
/// Capital allocation per trade. Squeeze entries are rare and the
/// "all-in shot" is the strategy's edge. 95 % rather than 100 % because
/// the SL is at -4 % and at full allocation the maintenance-margin
/// ratio at SL hit is right at the exchange's liquidation line — a
/// 5 % buffer means slippage on the SL fill can't trip a forced close
/// before our own SL does.
const ALLOCATION_PCT: Decimal = dec!(0.95);
const TIME_LIMIT_HOURS: i64 = 48;
const HISTORY_LEN: usize = 200;

pub struct SqueezeMomentumV1 {
    name: String,
    pairs: Vec<Pair>,
    history: HashMap<String, VecDeque<Candle>>,
    /// Per-pair counter of consecutive squeeze bars (BB inside KC). Used
    /// to require sustained compression before allowing a fire entry.
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

    /// Returns true when BB(20, 2σ) is fully contained in KC(20, 1.5×ATR).
    fn is_in_squeeze(history: &VecDeque<Candle>) -> Option<bool> {
        let closes = Self::closes(history);
        let highs = Self::highs(history);
        let lows = Self::lows(history);
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
        if event.exchange != Exchange::BitflyerCfd {
            return None;
        }
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }
        let key = event.pair.0.clone();
        self.push_candle(&key, event.candle.clone());

        // Need enough history for BB / KC / ATR / EMA / swing.
        let needed = BB_PERIOD
            .max(KC_PERIOD)
            .max(ATR_PERIOD + 1)
            .max(EMA_TRAIL_PERIOD)
            .max(SWING_LOOKBACK + 1);
        let history = self.history.get(&key)?;
        if history.len() < needed + 2 {
            return None;
        }

        let in_squeeze = Self::is_in_squeeze(history)?;
        let prev_count = *self.squeeze_count.get(&key).unwrap_or(&0);
        let new_count = if in_squeeze { prev_count + 1 } else { 0 };
        self.squeeze_count.insert(key.clone(), new_count);

        // Fire condition: we WERE in a sustained squeeze, and on this
        // bar BB has just exited the KC.
        let just_fired = !in_squeeze && prev_count >= SQUEEZE_BARS;
        if !just_fired {
            return None;
        }

        // Direction comes from momentum proxy = close - SMA(20).
        let closes = Self::closes(history);
        let sma20 = indicators::sma(&closes, BB_PERIOD)?;
        let mom_curr = closes[closes.len() - 1] - sma20;
        // Previous bar's momentum: rebuild SMA on closes[..len-1]
        let sma20_prev = indicators::sma(&closes[..closes.len() - 1], BB_PERIOD)?;
        let mom_prev = closes[closes.len() - 2] - sma20_prev;

        let entry = event.candle.close;
        let sl_offset = entry * SL_PCT;

        // Long: positive and rising momentum
        if mom_curr > Decimal::ZERO && mom_curr > mom_prev {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                entry_price: entry,
                stop_loss: entry - sl_offset,
                // Trailing exit handled in on_open_positions; fixed TP
                // is parked far away.
                take_profit: entry * dec!(1000),
                confidence: 0.55,
                timestamp: event.timestamp,
                allocation_pct: ALLOCATION_PCT,
                max_hold_until: Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS)),
            });
        }
        // Short: negative and falling momentum
        if mom_curr < Decimal::ZERO && mom_curr < mom_prev {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                entry_price: entry,
                stop_loss: entry + sl_offset,
                take_profit: entry / dec!(1000),
                confidence: 0.55,
                timestamp: event.timestamp,
                allocation_pct: ALLOCATION_PCT,
                max_hold_until: Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS)),
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
            // Update squeeze counter from warmup data so the live state
            // is consistent with reality after restart.
            if let Some(history) = self.history.get(&event.pair.0)
                && let Some(in_squeeze) = Self::is_in_squeeze(history)
            {
                let key = event.pair.0.clone();
                let prev = *self.squeeze_count.get(&key).unwrap_or(&0);
                let new = if in_squeeze { prev + 1 } else { 0 };
                self.squeeze_count.insert(key, new);
            }
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
        let closes = Self::closes(history);
        let Some(ema21) = indicators::ema(&closes, EMA_TRAIL_PERIOD) else {
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
            let trail_break = match pos.trade.direction {
                Direction::Long => close < ema21,
                Direction::Short => close > ema21,
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
    async fn no_signal_with_short_history() {
        let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let e = make_event("FX_BTC_JPY", dec!(10000000), dec!(10010000), dec!(9990000));
        assert!(s.on_price(&e).await.is_none());
    }

    #[tokio::test]
    async fn fires_long_after_squeeze_release_with_momentum() {
        let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        // 80 ultra-flat bars to force BB inside KC
        for _ in 0..80 {
            let _ = s
                .on_price(&make_event(
                    "FX_BTC_JPY",
                    dec!(10000000),
                    dec!(10000100),
                    dec!(9999900),
                ))
                .await;
        }
        // Sudden expansion: big up move that breaks BB outside KC AND
        // pushes close > SMA20 with rising momentum. Volume isn't tracked.
        let breakout = make_event(
            "FX_BTC_JPY",
            dec!(10500000),
            dec!(10600000),
            dec!(10000000),
        );
        let signal = s.on_price(&breakout).await;
        assert!(signal.is_some(), "expected long squeeze-momentum signal");
        let sig = signal.unwrap();
        assert_eq!(sig.direction, Direction::Long);
        // SL must be inside the 4% cap
        let cap = sig.entry_price * dec!(0.04);
        assert!(sig.entry_price - sig.stop_loss <= cap + dec!(0.001));
        assert!(sig.max_hold_until.is_some());
    }

    #[tokio::test]
    async fn open_positions_close_on_ema_break() {
        let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        // Build a rising history so EMA21 is around the highest values
        for i in 0..50 {
            let p = dec!(10000000) + Decimal::from(i) * dec!(10000);
            let _ = s
                .on_price(&make_event("FX_BTC_JPY", p, p + dec!(5000), p - dec!(5000)))
                .await;
        }
        let pos = make_position("sq", Direction::Long, dec!(10250000));
        // Sharp drop below EMA21
        let drop = make_event("FX_BTC_JPY", dec!(9000000), dec!(9050000), dec!(8950000));
        let _ = s.on_price(&drop).await;
        let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
        assert_eq!(exits.len(), 1, "expected EMA trailing exit");
        assert_eq!(exits[0].reason, StrategyExitReason::TrailingMa);
    }
}
