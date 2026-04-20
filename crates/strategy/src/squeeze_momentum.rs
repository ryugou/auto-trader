//! 攻めボラティリティ v1 (`squeeze_momentum_v1`).
//!
//! TTM Squeeze (John Carter) adapted for FX_BTC_JPY (1H). The textbook
//! TTM Squeeze fires when Bollinger Bands compress *inside* the Keltner
//! Channels for a sustained period and then re-expand outside, signaling
//! a volatility breakout. The bias direction is taken from the momentum
//! histogram (close vs SMA) — we use a simplified `close - SMA(20)` as
//! the momentum proxy ([TrendSpider TTM Squeeze guide](https://trendspider.com/learning-center/introduction-to-ttm-squeeze/),
//! [EBC Financial overview](https://www.ebc.com/forex/top-ways-to-master-the-ttm-squeeze-trading-strategy)).
//!
//! Moved from M5 → 1H to reduce false breakouts. Daily-bar-designed logic
//! suffered excessive whipsaws on 5-minute data.
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
//! ATR(14)-based: `min(ATR × 2.5 / entry, 5%)`. Squeeze releases can
//! whipsaw — 2.5× ATR gives the trade room to survive a single bad bar
//! without over-sizing the risk.
//!
//! ## Position sizing
//! `allocation_pct = min(1% / stop_loss_pct, 50%)`. With 2× account leverage
//! actual risk = `1% × 2 = 2%` of account per trade; caps at 50% to prevent
//! over-exposure.
//!
//! ## Take profit (dynamic, via `on_open_positions`)
//! Chandelier Exit (ATR-based trailing stop) replaces the old EMA(21)
//! trailing that cut profits short in volatile breakouts.
//!
//! - **Delay phase** (first 3 bars after entry): only the initial SL
//!   is active. No trailing — survives post-breakout retracement noise.
//! - **Trailing phase** (4th bar onward):
//!   - Long: stop = max(high over last 22 bars) - ATR(14) × 3.0
//!   - Short: stop = min(low over last 22 bars) + ATR(14) × 3.0
//! - 48-hour fail-safe via `max_hold_until`.
//!
//! Why Chandelier over EMA trailing: EMA tracks price average (ボラ非適応). ATR×3 tracks volatility itself — after a squeeze fires, ATR expands,
//! automatically widening the stop to let the breakout run. As the trend
//! matures and ATR stabilises, the stop tightens naturally.
//! (Perry Kaufman "Trading Systems and Methods", Van Tharp "Trade Your
//! Way to Financial Freedom", Brent Penfold "Universal Principles")
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
// EMA_TRAIL_PERIOD removed — replaced by Chandelier Exit (ATR-based).
/// Chandelier Exit lookback for highest-high / lowest-low.
const CHANDELIER_PERIOD: usize = 22;
/// Chandelier Exit ATR multiplier. 3.0 is the most widely backtested
/// value (Van Tharp, Chuck LeBeau).
const CHANDELIER_ATR_MULT: Decimal = dec!(3.0);
/// Number of bars after entry during which the trailing stop is NOT
/// applied (only the initial SL protects). Prevents premature exit
/// from post-breakout retracement noise (Brent Penfold).
const DELAY_BARS: usize = 3;
/// Lowered from 6 → 3 to increase trade frequency. The original
/// 6-bar requirement made squeeze detection too rare (4 trades in
/// 5 days vs donchian's 8). 3 bars still confirms a genuine
/// compression (not noise) while roughly doubling signal rate.
const SQUEEZE_BARS: usize = 3;
/// Bars looked back for the historical-needed minimum (kept for the
/// `len < needed + 2` guard even though SL is now ATR-based).
const SWING_LOOKBACK: usize = 5;
/// ATR multiplier for stop-loss. 2.5× ATR is wide enough to survive
/// post-squeeze whipsaws without over-extending the risk budget.
const ATR_MULT: Decimal = dec!(2.5);
/// Maximum stop-loss as a fraction of entry price.
const SL_CAP: Decimal = dec!(0.05);
/// Target risk per trade as an *unleveraged* fraction of account balance.
/// Target per-trade risk budget. The leverage-aware risk cap is enforced
/// by PositionSizer (which knows the actual account leverage), so this
/// value does not need manual adjustment when leverage changes.
const TARGET_RISK_PCT: Decimal = dec!(0.01);
/// Maximum allocation per trade.
const ALLOCATION_CAP: Decimal = dec!(0.50);
const TIME_LIMIT_HOURS: i64 = 48;
const HISTORY_LEN: usize = 200;
/// This strategy uses 1H candles (trend-following; M5 produced excessive
/// false breakouts on a daily-bar-designed logic).
const TIMEFRAME: &str = "H1";

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
        if event.candle.timeframe != TIMEFRAME {
            return None;
        }
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }
        let key = event.pair.0.clone();
        self.push_candle(&key, event.candle.clone());

        // Need enough history for BB / KC / ATR / Chandelier / swing.
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
        let highs = Self::highs(history);
        let lows = Self::lows(history);

        let sma20 = indicators::sma(&closes, BB_PERIOD)?;
        let mom_curr = closes[closes.len() - 1] - sma20;
        // Previous bar's momentum: rebuild SMA on closes[..len-1]
        let sma20_prev = indicators::sma(&closes[..closes.len() - 1], BB_PERIOD)?;
        let mom_prev = closes[closes.len() - 2] - sma20_prev;

        let entry = event.candle.close;
        // ATR-based stop-loss, capped at SL_CAP.
        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;
        let stop_loss_pct = (atr * ATR_MULT / entry).min(SL_CAP);
        if stop_loss_pct <= Decimal::ZERO {
            return None; // ATR=0, no volatility to trade
        }
        // Risk-linked allocation: risk at most TARGET_RISK_PCT of account.
        let allocation_pct = (TARGET_RISK_PCT / stop_loss_pct).min(ALLOCATION_CAP);

        // Long: positive and rising momentum
        if mom_curr > Decimal::ZERO && mom_curr > mom_prev {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                stop_loss_pct,
                // Trailing exit handled in on_open_positions; dynamic exit strategy.
                take_profit_pct: None,
                confidence: 0.55,
                timestamp: event.timestamp,
                allocation_pct,
                max_hold_until: Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS)),
            });
        }
        // Short: negative and falling momentum
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
            if event.exchange != Exchange::BitflyerCfd {
                continue;
            }
            if event.candle.timeframe != TIMEFRAME {
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
        if event.candle.timeframe != TIMEFRAME {
            return Vec::new();
        }
        let key = event.pair.0.clone();
        let Some(history) = self.history.get(&key) else {
            return Vec::new();
        };
        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let closes = Self::closes(history);
        let close = event.candle.close;

        // ATR for Chandelier Exit — need enough history.
        let Some(atr) = indicators::atr(&highs, &lows, &closes, ATR_PERIOD) else {
            return Vec::new();
        };
        if atr <= Decimal::ZERO {
            return Vec::new(); // Perfectly flat — no meaningful trailing stop.
        }

        // Highest high / lowest low over CHANDELIER_PERIOD bars.
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

            // Delay phase: count bars since entry. During the first
            // DELAY_BARS bars, only the fixed SL (managed by the position
            // monitor) protects — don't apply the trailing stop yet.
            // Count completed bars since entry by flooring entry_at to the H1 period start,
            // so the delay phase is consistent regardless of exact fill timing within the candle.
            use chrono::Timelike;
            let entry_hour = pos
                .trade
                .entry_at
                .with_minute(0)
                .unwrap()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap();
            // Count completed bars since entry. Candle timestamps are period
            // starts; entry_hour is floored to the same boundary. Use >= so the
            // candle whose period contains the entry is counted as bar 0.
            // DELAY_BARS=3 means bars 0,1,2 are in delay; bar 3+ is trailing.
            let bars_held = history
                .iter()
                .filter(|c| c.timestamp >= entry_hour)
                .count()
                .saturating_sub(1); // don't count the entry bar itself
            if bars_held < DELAY_BARS {
                continue;
            }

            // Chandelier Exit trailing stop.
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
mod tests {
    use super::*;
    use auto_trader_core::types::{Candle, Pair, Trade, TradeStatus};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_event_at(
        pair: &str,
        close: Decimal,
        high: Decimal,
        low: Decimal,
        ts: chrono::DateTime<Utc>,
    ) -> PriceEvent {
        PriceEvent {
            pair: Pair::new(pair),
            exchange: Exchange::BitflyerCfd,
            timestamp: ts,
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
                timestamp: ts,
            },
            indicators: HashMap::new(),
        }
    }

    fn make_event(pair: &str, close: Decimal, high: Decimal, low: Decimal) -> PriceEvent {
        make_event_at(pair, close, high, low, Utc::now())
    }

    fn make_position(
        strategy: &str,
        direction: Direction,
        entry: Decimal,
        entry_at: chrono::DateTime<Utc>,
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
                stop_loss: dec!(0),
                take_profit: None,
                quantity: dec!(0.001),
                leverage: dec!(2),
                fees: dec!(0),
                entry_at,
                exit_at: None,
                pnl_amount: None,
                exit_reason: None,
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

    /// M5 candles must be silently ignored — this strategy runs on H1.
    #[tokio::test]
    async fn ignores_non_h1_timeframe() {
        let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let mut e = make_event("FX_BTC_JPY", dec!(10000000), dec!(10010000), dec!(9990000));
        e.candle.timeframe = "M5".to_string();
        for _ in 0..100 {
            assert!(s.on_price(&e).await.is_none());
        }
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
        let breakout = make_event("FX_BTC_JPY", dec!(10500000), dec!(10600000), dec!(10000000));
        let signal = s.on_price(&breakout).await;
        assert!(signal.is_some(), "expected long squeeze-momentum signal");
        let sig = signal.unwrap();
        assert_eq!(sig.direction, Direction::Long);
        // ATR-based SL: positive and at most SL_CAP (5%).
        assert!(sig.stop_loss_pct > Decimal::ZERO);
        assert!(sig.stop_loss_pct <= dec!(0.05));
        // Risk-linked allocation: positive and at most ALLOCATION_CAP (50%).
        assert!(
            sig.allocation_pct > Decimal::ZERO && sig.allocation_pct <= dec!(0.50),
            "allocation must be in (0, 50%], got {}",
            sig.allocation_pct
        );
        // Dynamic exit strategy → TP is None
        assert!(sig.take_profit_pct.is_none());
        assert!(sig.max_hold_until.is_some());
    }

    #[tokio::test]
    async fn open_positions_close_on_chandelier_break() {
        use chrono::Duration;

        let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let base_ts = Utc::now() - Duration::hours(60);

        // Build a rising history (50 bars). Each bar = 1H.
        // High range ~10000 per bar → ATR ≈ 10000.
        // Highest high in last 22 bars ≈ 10,490,000 + 5000 = 10,495,000
        // Chandelier stop (long) = 10,495,000 - ATR(14)*3 ≈ 10,495,000 - 30,000 = 10,465,000
        for i in 0..50 {
            let ts = base_ts + Duration::hours(i);
            let p = dec!(10000000) + Decimal::from(i) * dec!(10000);
            let _ = s
                .on_price(&make_event_at(
                    "FX_BTC_JPY",
                    p,
                    p + dec!(5000),
                    p - dec!(5000),
                    ts,
                ))
                .await;
        }

        // Position entered at bar 40 (10 bars ago → well past DELAY_BARS=3).
        let entry_ts = base_ts + Duration::hours(40);
        let pos = make_position("sq", Direction::Long, dec!(10400000), entry_ts);

        // Sharp drop well below Chandelier stop.
        let drop_ts = base_ts + Duration::hours(50);
        let drop = make_event_at(
            "FX_BTC_JPY",
            dec!(9000000),
            dec!(9050000),
            dec!(8950000),
            drop_ts,
        );
        let _ = s.on_price(&drop).await;
        let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
        assert_eq!(exits.len(), 1, "expected Chandelier trailing exit");
        assert_eq!(exits[0].reason, StrategyExitReason::TrailingMa);
    }

    #[tokio::test]
    async fn chandelier_does_not_exit_during_delay_phase() {
        use chrono::Duration;

        let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let base_ts = Utc::now() - Duration::hours(60);

        // Build 50 bars of rising history.
        for i in 0..50 {
            let ts = base_ts + Duration::hours(i);
            let p = dec!(10000000) + Decimal::from(i) * dec!(10000);
            let _ = s
                .on_price(&make_event_at(
                    "FX_BTC_JPY",
                    p,
                    p + dec!(5000),
                    p - dec!(5000),
                    ts,
                ))
                .await;
        }

        // Position entered at bar 49 (1 bar ago → within DELAY_BARS=3).
        let entry_ts = base_ts + Duration::hours(49);
        let pos = make_position("sq", Direction::Long, dec!(10490000), entry_ts);

        // Drop below Chandelier stop — but still in delay phase.
        let drop_ts = base_ts + Duration::hours(50);
        let drop = make_event_at(
            "FX_BTC_JPY",
            dec!(9000000),
            dec!(9050000),
            dec!(8950000),
            drop_ts,
        );
        let _ = s.on_price(&drop).await;
        let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
        assert_eq!(
            exits.len(),
            0,
            "should NOT exit during delay phase (first 3 bars)"
        );
    }

    #[tokio::test]
    async fn chandelier_exits_at_boundary_of_delay_phase() {
        use chrono::Duration;

        let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let base_ts = Utc::now() - Duration::hours(60);

        // 50 bars of rising history.
        for i in 0..50 {
            let ts = base_ts + Duration::hours(i);
            let p = dec!(10000000) + Decimal::from(i) * dec!(10000);
            let _ = s
                .on_price(&make_event_at(
                    "FX_BTC_JPY",
                    p,
                    p + dec!(5000),
                    p - dec!(5000),
                    ts,
                ))
                .await;
        }

        // Position entered at bar 47 (exactly 3 bars ago = DELAY_BARS).
        // bars_held = 3 (bars 48, 49, 50 are after entry).
        // `bars_held < DELAY_BARS` is `3 < 3` = false → trailing IS active.
        let entry_ts = base_ts + Duration::hours(47);
        let pos = make_position("sq", Direction::Long, dec!(10470000), entry_ts);

        // Drop below Chandelier stop — should exit (delay phase over).
        let drop_ts = base_ts + Duration::hours(50);
        let drop = make_event_at(
            "FX_BTC_JPY",
            dec!(9000000),
            dec!(9050000),
            dec!(8950000),
            drop_ts,
        );
        let _ = s.on_price(&drop).await;
        let exits = s.on_open_positions(std::slice::from_ref(&pos), &drop).await;
        assert_eq!(
            exits.len(),
            1,
            "at bars_held == DELAY_BARS (boundary), trailing should be active"
        );
    }

    #[tokio::test]
    async fn chandelier_no_exit_when_atr_is_zero() {
        use chrono::Duration;
        let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let base_ts = Utc::now() - Duration::hours(60);
        // Build 50 perfectly flat bars so ATR(14) = 0.
        let p = dec!(10000000);
        for i in 0..50 {
            let ts = base_ts + Duration::hours(i);
            let _ = s.on_price(&make_event_at("FX_BTC_JPY", p, p, p, ts)).await;
        }
        // Position entered well into the flat history (bar 40).
        let entry_ts = base_ts + Duration::hours(40);
        let pos = make_position("sq", Direction::Long, p, entry_ts);

        // Test on_open_positions with a FLAT event (still no volatility).
        // ATR on 51 perfectly flat bars = 0, so on_open_positions should
        // return early and NOT emit a chandelier exit.
        let flat_event = make_event_at("FX_BTC_JPY", p, p, p, base_ts + Duration::hours(50));
        let _ = s.on_price(&flat_event).await;
        let exits = s
            .on_open_positions(std::slice::from_ref(&pos), &flat_event)
            .await;
        assert_eq!(
            exits.len(),
            0,
            "ATR=0 should produce no chandelier exits, got {} exits",
            exits.len()
        );
    }

    #[tokio::test]
    async fn chandelier_exits_short_on_break() {
        use chrono::Duration;
        let mut s = SqueezeMomentumV1::new("sq".to_string(), vec![Pair::new("FX_BTC_JPY")]);
        let base_ts = Utc::now() - Duration::hours(60);
        // 50 bars of FALLING history → lowest_low is recent, short position should trail.
        for i in 0..50 {
            let ts = base_ts + Duration::hours(i);
            let p = dec!(10000000) - Decimal::from(i) * dec!(10000);
            let _ = s
                .on_price(&make_event_at(
                    "FX_BTC_JPY",
                    p,
                    p + dec!(5000),
                    p - dec!(5000),
                    ts,
                ))
                .await;
        }
        // Short position entered at bar 40 (well past delay).
        let entry_ts = base_ts + Duration::hours(40);
        let pos = make_position("sq", Direction::Short, dec!(9600000), entry_ts);
        // Sharp RISE above Chandelier stop (lowest_low + ATR×3).
        let spike_ts = base_ts + Duration::hours(50);
        let spike = make_event_at(
            "FX_BTC_JPY",
            dec!(11000000),
            dec!(11050000),
            dec!(10950000),
            spike_ts,
        );
        let _ = s.on_price(&spike).await;
        let exits = s
            .on_open_positions(std::slice::from_ref(&pos), &spike)
            .await;
        assert_eq!(
            exits.len(),
            1,
            "expected Short chandelier exit on upward break"
        );
        assert_eq!(exits[0].reason, StrategyExitReason::TrailingMa);
    }
}
