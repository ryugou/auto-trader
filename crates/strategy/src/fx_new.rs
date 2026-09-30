//! Fast USD/JPY breakout strategy used by the `FX新` paper account.
//!
//! M5 input is aggregated into completed M15 candles. Entries require a
//! 16-bar Donchian breakout, expanding ATR, and agreement with the last
//! completed H1 SMA20/SMA50 trend. Position size targets a 1% equity loss at
//! the initial 2 ATR stop (subject to the executor's broker margin cap).

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy, StrategyExitReason};
use auto_trader_core::types::{Candle, Direction, Pair, Position, Signal};
use auto_trader_market::indicators;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const ENTRY_CHANNEL: usize = 16;
const EXIT_CHANNEL: usize = 8;
const ATR_PERIOD: usize = 14;
const ATR_BASELINE: usize = 20;
const H1_FAST: usize = 20;
const H1_SLOW: usize = 50;
const ATR_MULT: Decimal = dec!(2);
const SL_CAP: Decimal = dec!(0.02);
const TARGET_RISK_PCT: Decimal = dec!(0.01);
const GMO_FX_LEVERAGE: Decimal = dec!(10);
// Maximum number of M15 (and H1) bars kept in memory: five trading days of
// M15 bars. This is a cap on in-process history, not a guarantee of what is
// available after a restart. Startup warmup (`WARMUP_LIMIT = 200` M5 bars in
// crates/app/src/main.rs) only rebuilds about 66 M15 bars (~16.7 hours), so
// the 1R-reached state of a trade entered before that window is lost on
// restart and its trailing exit stays gated until 1R is observed again.
const HISTORY_LEN: usize = 480;

pub struct FxNewV1 {
    name: String,
    pairs: Vec<Pair>,
    m5_pending: HashMap<String, Vec<Candle>>,
    m15: HashMap<String, VecDeque<Candle>>,
    h1: HashMap<String, VecDeque<Candle>>,
}

impl FxNewV1 {
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
        self.pairs.iter().any(|p| p == &event.pair)
    }

    fn push_limited(history: &mut VecDeque<Candle>, candle: Candle) {
        history.push_back(candle);
        while history.len() > HISTORY_LEN {
            history.pop_front();
        }
    }

    fn push_h1(&mut self, event: &PriceEvent) {
        let history = self.h1.entry(event.pair.0.clone()).or_default();
        Self::push_limited(history, event.candle.clone());
    }

    /// Return a completed M15 candle after receiving its three M5 components.
    fn aggregate_m15(&mut self, event: &PriceEvent) -> Option<Candle> {
        let ts = event.candle.timestamp.timestamp();
        let bucket = ts - ts.rem_euclid(900);
        let pending = self.m5_pending.entry(event.pair.0.clone()).or_default();

        if pending.first().is_some_and(|c| {
            c.timestamp.timestamp() - c.timestamp.timestamp().rem_euclid(900) != bucket
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
            // The aggregation timestamp is the start of its M15 bucket. Do
            // not count the entry candle's pre-fill high/low as post-entry MFE.
            .filter(|c| c.timestamp > trade.entry_at)
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
        let candle = self.aggregate_m15(event)?;
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
            confidence: 0.65,
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
            } else if event.candle.timeframe == "M5"
                && let Some(candle) = self.aggregate_m15(event)
            {
                let history = self.m15.entry(event.pair.0.clone()).or_default();
                Self::push_limited(history, candle);
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
        let Some(history) = self.m15.get(&event.pair.0) else {
            return Vec::new();
        };
        let highs: Vec<Decimal> = history.iter().map(|c| c.high).collect();
        let lows: Vec<Decimal> = history.iter().map(|c| c.low).collect();
        let Some((exit_low, exit_high)) =
            indicators::donchian_channel(&highs, &lows, EXIT_CHANNEL, false)
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
mod tests {
    use super::*;
    use auto_trader_core::types::{Exchange, ExitReason, Trade, TradeStatus};
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    #[test]
    fn one_percent_risk_sizing_includes_leverage() {
        assert_eq!(FxNewV1::allocation_for(dec!(0.01)), dec!(0.1));
        assert_eq!(FxNewV1::allocation_for(dec!(0.02)), dec!(0.05));
    }

    fn candle(timestamp: chrono::DateTime<Utc>, high: Decimal, low: Decimal) -> Candle {
        Candle {
            pair: Pair::new("USD_JPY"),
            exchange: Exchange::GmoFx,
            timeframe: "M15".to_string(),
            open: dec!(100),
            high,
            low,
            close: dec!(100),
            volume: Some(0),
            best_bid: None,
            best_ask: None,
            timestamp,
        }
    }

    fn long_position(entry_at: chrono::DateTime<Utc>) -> Position {
        Position {
            trade: Trade {
                id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                strategy_name: "fx_new_v1".to_string(),
                pair: Pair::new("USD_JPY"),
                exchange: Exchange::GmoFx,
                direction: Direction::Long,
                entry_price: dec!(100),
                exit_price: None,
                stop_loss: dec!(99),
                take_profit: None,
                quantity: dec!(1),
                leverage: dec!(10),
                fees: Decimal::ZERO,
                entry_at,
                exit_at: None,
                pnl_amount: None,
                exit_reason: None::<ExitReason>,
                status: TradeStatus::Open,
                max_hold_until: None,
                exchange_position_id: None,
                stop_order_id: None,
            },
        }
    }

    #[test]
    fn one_r_gate_remains_active_after_price_retraces() {
        let entry_at = Utc::now();
        let position = long_position(entry_at);
        let history = VecDeque::from([
            candle(entry_at + Duration::minutes(15), dec!(101.2), dec!(100)),
            candle(entry_at + Duration::minutes(30), dec!(100.7), dec!(100.2)),
        ]);
        let current = candle(entry_at + Duration::minutes(45), dec!(100.5), dec!(99.8));

        assert!(FxNewV1::reached_one_r(&position, &history, &current));
    }

    #[test]
    fn one_r_gate_ignores_pre_fill_extreme_on_entry_bucket() {
        let entry_at = Utc::now();
        let position = long_position(entry_at);
        let history = VecDeque::from([candle(
            entry_at - Duration::minutes(5),
            dec!(101.2),
            dec!(99.8),
        )]);
        let current = candle(entry_at + Duration::minutes(5), dec!(100.8), dec!(99.9));

        assert!(!FxNewV1::reached_one_r(&position, &history, &current));
    }

    fn base_time() -> chrono::DateTime<Utc> {
        use chrono::TimeZone;
        // Monday 00:00 UTC, aligned to a 15-minute bucket boundary.
        Utc.with_ymd_and_hms(2026, 1, 5, 0, 0, 0).unwrap()
    }

    fn price_event(
        pair: &str,
        timeframe: &str,
        timestamp: chrono::DateTime<Utc>,
        ohlc: (Decimal, Decimal, Decimal, Decimal),
        volume: u64,
    ) -> PriceEvent {
        let (open, high, low, close) = ohlc;
        PriceEvent {
            pair: Pair::new(pair),
            exchange: Exchange::GmoFx,
            candle: Candle {
                pair: Pair::new(pair),
                exchange: Exchange::GmoFx,
                timeframe: timeframe.to_string(),
                open,
                high,
                low,
                close,
                volume: Some(volume),
                best_bid: None,
                best_ask: None,
                timestamp,
            },
            indicators: HashMap::new(),
            timestamp,
        }
    }

    fn m5(minute: i64, ohlc: (Decimal, Decimal, Decimal, Decimal), volume: u64) -> PriceEvent {
        price_event(
            "USD_JPY",
            "M5",
            base_time() + Duration::minutes(minute),
            ohlc,
            volume,
        )
    }

    fn strategy() -> FxNewV1 {
        FxNewV1::new("fx_new_v1".to_string(), vec![Pair::new("USD_JPY")])
    }

    #[test]
    fn aggregate_m15_combines_three_m5_bars_into_one_bucket_candle() {
        let mut s = strategy();

        assert!(
            s.aggregate_m15(&m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10))
                .is_none()
        );
        assert!(
            s.aggregate_m15(&m5(5, (dec!(100.5), dec!(103), dec!(100), dec!(102)), 20))
                .is_none()
        );
        let m15 = s
            .aggregate_m15(&m5(10, (dec!(102), dec!(102.5), dec!(98), dec!(101)), 30))
            .expect("third M5 bar completes the M15 candle");

        assert_eq!(m15.timeframe, "M15");
        assert_eq!(m15.timestamp, base_time());
        assert_eq!(m15.open, dec!(100));
        assert_eq!(m15.close, dec!(101));
        assert_eq!(m15.high, dec!(103));
        assert_eq!(m15.low, dec!(98));
        assert_eq!(m15.volume, Some(60));
    }

    #[test]
    fn aggregate_m15_ignores_duplicate_m5_timestamp() {
        let mut s = strategy();
        let bar = (dec!(100), dec!(101), dec!(99), dec!(100));

        assert!(s.aggregate_m15(&m5(0, bar, 1)).is_none());
        assert!(s.aggregate_m15(&m5(0, bar, 1)).is_none());
        assert!(
            s.aggregate_m15(&m5(5, bar, 1)).is_none(),
            "duplicate 00:00 must not count as a third bar"
        );
    }

    #[test]
    fn aggregate_m15_discards_incomplete_previous_bucket() {
        let mut s = strategy();

        // 00:00 bucket only receives 00:05 and 00:10 (00:00 missing), with an
        // extreme high that must not leak into the next bucket.
        assert!(
            s.aggregate_m15(&m5(5, (dec!(100), dec!(200), dec!(50), dec!(100)), 1000))
                .is_none()
        );
        assert!(
            s.aggregate_m15(&m5(10, (dec!(100), dec!(200), dec!(50), dec!(100)), 1000))
                .is_none()
        );

        let ohlc = (dec!(100), dec!(101), dec!(99), dec!(100));
        assert!(s.aggregate_m15(&m5(15, ohlc, 1)).is_none());
        assert!(s.aggregate_m15(&m5(20, ohlc, 1)).is_none());
        let m15 = s
            .aggregate_m15(&m5(25, ohlc, 1))
            .expect("00:15 bucket completes");

        assert_eq!(m15.timestamp, base_time() + Duration::minutes(15));
        assert_eq!(m15.high, dec!(101));
        assert_eq!(m15.low, dec!(99));
        assert_eq!(m15.volume, Some(3));
    }

    #[test]
    fn allocation_is_clipped_to_one_for_tiny_stop_loss() {
        assert_eq!(FxNewV1::allocation_for(dec!(0.0005)), Decimal::ONE);
    }

    fn push_h1_closes(s: &mut FxNewV1, closes: impl IntoIterator<Item = Decimal>) {
        for (i, close) in closes.into_iter().enumerate() {
            let event = price_event(
                "USD_JPY",
                "H1",
                base_time() + Duration::hours(i as i64),
                (close, close, close, close),
                0,
            );
            s.push_h1(&event);
        }
    }

    #[test]
    fn h1_direction_is_none_with_fewer_than_fifty_bars() {
        let mut s = strategy();
        push_h1_closes(&mut s, (1..=49).map(Decimal::from));

        assert_eq!(s.h1_direction("USD_JPY"), None);
    }

    #[test]
    fn h1_direction_is_long_when_fast_sma_above_slow_sma() {
        let mut s = strategy();
        push_h1_closes(&mut s, (1..=50).map(Decimal::from));

        assert_eq!(s.h1_direction("USD_JPY"), Some(Direction::Long));
    }

    #[test]
    fn h1_direction_is_short_when_fast_sma_below_slow_sma() {
        let mut s = strategy();
        push_h1_closes(&mut s, (1..=50).rev().map(Decimal::from));

        assert_eq!(s.h1_direction("USD_JPY"), Some(Direction::Short));
    }

    #[tokio::test]
    async fn on_price_ignores_other_pairs() {
        let mut s = strategy();
        let event = price_event(
            "EUR_USD",
            "M5",
            base_time(),
            (dec!(1), dec!(1), dec!(1), dec!(1)),
            0,
        );

        assert!(s.on_price(&event).await.is_none());
        assert!(s.m5_pending.is_empty(), "other pair must not be buffered");
    }

    #[tokio::test]
    async fn on_price_ignores_timeframes_other_than_m5_and_h1() {
        let mut s = strategy();
        for timeframe in ["M1", "M15", "D1"] {
            let event = price_event(
                "USD_JPY",
                timeframe,
                base_time(),
                (dec!(100), dec!(100), dec!(100), dec!(100)),
                0,
            );
            assert!(s.on_price(&event).await.is_none(), "{timeframe}");
        }
        assert!(s.m5_pending.is_empty(), "non-M5 must not be buffered");
        assert!(s.h1.is_empty(), "non-H1 must not be stored as H1");
    }

    /// Ten M15 bars after entry: lows at 99.5, highs at `peak_high` for the
    /// first bar and 100.5 for the rest.
    fn history_after_entry(
        entry_at: chrono::DateTime<Utc>,
        peak_high: Decimal,
    ) -> VecDeque<Candle> {
        (1..=10)
            .map(|i| {
                let high = if i == 1 { peak_high } else { dec!(100.5) };
                candle(entry_at + Duration::minutes(15 * i), high, dec!(99.5))
            })
            .collect()
    }

    #[tokio::test]
    async fn on_open_positions_holds_before_one_r_even_below_exit_channel() {
        let mut s = strategy();
        let entry_at = base_time();
        let position = long_position(entry_at);
        // 1R target is 101 (entry 100, stop 99); highs stay below it.
        s.m15.insert(
            "USD_JPY".to_string(),
            history_after_entry(entry_at, dec!(100.5)),
        );
        // Close 99.0 is below the 8-bar exit low (99.5) and its high stays under 1R.
        let event = price_event(
            "USD_JPY",
            "M5",
            entry_at + Duration::minutes(15 * 11),
            (dec!(99.8), dec!(100.2), dec!(99), dec!(99)),
            0,
        );

        let exits = s
            .on_open_positions(std::slice::from_ref(&position), &event)
            .await;

        assert!(exits.is_empty(), "1R not reached: trailing exit is gated");
    }

    #[tokio::test]
    async fn on_open_positions_exits_below_channel_once_one_r_reached() {
        let mut s = strategy();
        let entry_at = base_time();
        let position = long_position(entry_at);
        // First post-entry bar reaches the 1R target (101).
        s.m15.insert(
            "USD_JPY".to_string(),
            history_after_entry(entry_at, dec!(101)),
        );
        let event = price_event(
            "USD_JPY",
            "M5",
            entry_at + Duration::minutes(15 * 11),
            (dec!(99.8), dec!(100.2), dec!(99), dec!(99)),
            0,
        );

        let exits = s
            .on_open_positions(std::slice::from_ref(&position), &event)
            .await;

        assert_eq!(exits.len(), 1);
        assert_eq!(exits[0].trade_id, position.trade.id);
        assert_eq!(exits[0].close_price, dec!(99));
    }
}
