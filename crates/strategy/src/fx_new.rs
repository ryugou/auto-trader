//! Fast USD/JPY breakout strategy used by the `FX新` paper account.
//!
//! M5 input is aggregated into completed M15 candles. Entries require a
//! 16-bar Donchian breakout, expanding ATR, and agreement with the last
//! completed H1 SMA20/SMA50 trend. Position size targets a 1% equity loss at
//! the initial 2 ATR stop (subject to the executor's broker margin cap).

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy, StrategyExitReason};
use auto_trader_core::types::{Candle, Direction, Exchange, Pair, Position, Signal};
use auto_trader_market::indicators;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const ENTRY_CHANNEL: usize = 16;
const EXIT_CHANNEL: usize = 8;
const M15_MINUTES: i64 = 15;
const ATR_PERIOD: usize = 14;
const ATR_BASELINE: usize = 20;
const H1_FAST: usize = 20;
const H1_SLOW: usize = 50;
const ATR_MULT: Decimal = dec!(2);
const SL_CAP: Decimal = dec!(0.02);
const TARGET_RISK_PCT: Decimal = dec!(0.01);
pub const GMO_FX_LEVERAGE: Decimal = dec!(10);
// `Exchange` is `Copy`, so this can be a `const` rather than a `fn`. Startup
// validation (crates/app/src/startup.rs) compares an account's configured
// exchange against this single source of truth instead of duplicating the
// `Exchange::GmoFx` literal.
pub const FX_NEW_EXCHANGE: Exchange = Exchange::GmoFx;
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
        // This strategy is wired to the gmo_fx account only; without the
        // exchange check, OANDA ticks for the same pair would mix into the
        // same M5/M15/H1 history as GMO FX and corrupt breakout/trend state.
        event.exchange == FX_NEW_EXCHANGE && self.pairs.iter().any(|p| p == &event.pair)
    }

    fn push_limited(history: &mut VecDeque<Candle>, candle: Candle) {
        history.push_back(candle);
        while history.len() > HISTORY_LEN {
            history.pop_front();
        }
    }

    /// Reject a candle with any non-positive OHLC value before it reaches
    /// history. `entry = candle.close` (in `on_price`) is later used as a
    /// divisor for `stop_loss_pct`, so a non-positive close would panic on
    /// that division; a non-positive open/high/low likewise indicates
    /// corrupt upstream market data rather than a real price. Treating the
    /// whole bar as invalid (instead of clamping it) keeps corrupt data out
    /// of the Donchian channel / ATR / SMA state it would otherwise poison.
    fn is_valid_candle(candle: &Candle) -> bool {
        candle.open > Decimal::ZERO
            && candle.high > Decimal::ZERO
            && candle.low > Decimal::ZERO
            && candle.close > Decimal::ZERO
    }

    fn push_h1(&mut self, event: &PriceEvent) {
        if !Self::is_valid_candle(&event.candle) {
            tracing::warn!(
                "fx_new: non-positive OHLC for H1 {} at {}: open={} high={} low={} close={}, skipping",
                event.pair.0,
                event.candle.timestamp,
                event.candle.open,
                event.candle.high,
                event.candle.low,
                event.candle.close
            );
            return;
        }
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

    /// End of an M15 bar's period; `timestamp` is the bucket start.
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
            // The M15 bar's period ends 15 minutes after its bucket-start
            // timestamp. Include a bar if its period ended after entry_at, so the
            // bar covering the entry moment itself is not always excluded (entry
            // fires seconds after the bar completes, i.e. seconds into the *next*
            // bucket).
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
        // Validate the M5 bar itself before it reaches `aggregate_m15`. The
        // aggregate's open/close come from only the first/third M5 bars and
        // its high/low are a min/max across all three, so a corrupt value on
        // the *middle* bar (or a corrupt high/low that isn't the extreme)
        // would never surface in the aggregated M15 candle's own OHLC and
        // would slip past the post-aggregation `is_valid_candle` check below.
        if !Self::is_valid_candle(&event.candle) {
            tracing::warn!(
                "fx_new: non-positive OHLC for M5 {} at {}: open={} high={} low={} close={}, skipping aggregation",
                event.pair.0,
                event.candle.timestamp,
                event.candle.open,
                event.candle.high,
                event.candle.low,
                event.candle.close
            );
            return None;
        }
        let candle = self.aggregate_m15(event)?;
        if !Self::is_valid_candle(&candle) {
            tracing::warn!(
                "fx_new: non-positive OHLC for {} at {}: open={} high={} low={} close={}, skipping",
                candle.pair,
                candle.timestamp,
                candle.open,
                candle.high,
                candle.low,
                candle.close
            );
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
            } else if event.candle.timeframe == "M5" && !Self::is_valid_candle(&event.candle) {
                // Same per-M5 validation as `on_price`: catch a corrupt
                // middle/non-extreme M5 value here, before it ever reaches
                // `aggregate_m15`, since the aggregated M15 candle's own
                // OHLC would not necessarily reflect it.
                tracing::warn!(
                    "fx_new: non-positive OHLC for M5 {} at {}: open={} high={} low={} close={}, skipping aggregation during warmup",
                    event.pair.0,
                    event.candle.timestamp,
                    event.candle.open,
                    event.candle.high,
                    event.candle.low,
                    event.candle.close
                );
            } else if event.candle.timeframe == "M5"
                && let Some(candle) = self.aggregate_m15(event)
            {
                if Self::is_valid_candle(&candle) {
                    let history = self.m15.entry(event.pair.0.clone()).or_default();
                    Self::push_limited(history, candle);
                } else {
                    tracing::warn!(
                        "fx_new: non-positive OHLC for {} at {}: open={} high={} low={} close={}, skipping during warmup",
                        candle.pair,
                        candle.timestamp,
                        candle.open,
                        candle.high,
                        candle.low,
                        candle.close
                    );
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
            tracing::warn!(
                "fx_new: non-positive OHLC for {} at {}: open={} high={} low={} close={}, skipping exit check",
                event.pair.0,
                event.candle.timestamp,
                event.candle.open,
                event.candle.high,
                event.candle.low,
                event.candle.close
            );
            return Vec::new();
        }
        let Some(history) = self.m15.get(&event.pair.0) else {
            return Vec::new();
        };
        // Only M15 bars whose period ended at or before this M5 candle's start
        // feed the exit channel. The engine calls `on_price` before
        // `on_open_positions` on the same event, so on the tick that completes
        // an M15 bucket the last history bar already contains the current M5
        // candle's high/low; including it would make `close < exit_low`
        // (or `close > exit_high`) impossible and absorb the breakdown.
        // include_current=true keeps the channel from lagging by one bar
        // once the filtered last bar is genuinely prior to the current M5.
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

    fn short_position(entry_at: chrono::DateTime<Utc>) -> Position {
        Position {
            trade: Trade {
                id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                strategy_name: "fx_new_v1".to_string(),
                pair: Pair::new("USD_JPY"),
                exchange: Exchange::GmoFx,
                direction: Direction::Short,
                entry_price: dec!(100),
                exit_price: None,
                stop_loss: dec!(101),
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
    fn one_r_gate_ignores_bar_fully_completed_before_entry() {
        let entry_at = Utc::now();
        let position = long_position(entry_at);
        // This bar's period ends at entry_at - 5min, strictly before entry_at —
        // must be excluded even though its high would otherwise reach 1R.
        let history = VecDeque::from([candle(
            entry_at - Duration::minutes(20),
            dec!(101.2),
            dec!(99.8),
        )]);
        let current = candle(entry_at + Duration::minutes(5), dec!(100.8), dec!(99.9));

        assert!(!FxNewV1::reached_one_r(&position, &history, &current));
    }

    #[test]
    fn one_r_gate_includes_bar_covering_entry_timestamp() {
        let entry_at = Utc::now();
        let position = long_position(entry_at);
        // Bucket [entry_at - 5min, entry_at + 10min) covers entry_at; its high
        // reaches the 1R target (101) and must now count.
        let history = VecDeque::from([candle(
            entry_at - Duration::minutes(5),
            dec!(101.2),
            dec!(99.8),
        )]);
        let current = candle(entry_at + Duration::minutes(10), dec!(100.1), dec!(99.9));

        assert!(FxNewV1::reached_one_r(&position, &history, &current));
    }

    #[test]
    fn one_r_gate_includes_bar_covering_entry_timestamp_for_short() {
        let entry_at = Utc::now();
        let position = short_position(entry_at);
        // Bucket [entry_at - 5min, entry_at + 10min) covers entry_at; its low
        // reaches the 1R target (99, entry 100 - stop 101 = -1R) and must now count.
        let history = VecDeque::from([candle(
            entry_at - Duration::minutes(5),
            dec!(100.2),
            dec!(98.8),
        )]);
        let current = candle(entry_at + Duration::minutes(10), dec!(99.9), dec!(99.1));

        assert!(FxNewV1::reached_one_r(&position, &history, &current));
    }

    #[test]
    fn one_r_gate_excludes_bar_whose_period_ends_exactly_at_entry_at() {
        let entry_at = Utc::now();
        let position = long_position(entry_at);
        // Bar's period is [entry_at - 15min, entry_at); it ends exactly at
        // entry_at, which the strict `>` comparison in reached_one_r excludes.
        let history = VecDeque::from([candle(
            entry_at - Duration::minutes(15),
            dec!(101.2),
            dec!(99.8),
        )]);
        let current = candle(entry_at + Duration::minutes(5), dec!(100.5), dec!(99.9));

        assert!(!FxNewV1::reached_one_r(&position, &history, &current));
    }

    #[test]
    fn one_r_gate_includes_bar_whose_period_ends_one_second_after_entry_at() {
        let entry_at = Utc::now();
        let position = long_position(entry_at);
        // Bar's period ends at entry_at + 1s, one second after entry_at —
        // must be included.
        let history = VecDeque::from([candle(
            entry_at - Duration::minutes(15) + Duration::seconds(1),
            dec!(101.2),
            dec!(99.8),
        )]);
        let current = candle(entry_at + Duration::minutes(5), dec!(100.5), dec!(99.9));

        assert!(FxNewV1::reached_one_r(&position, &history, &current));
    }

    #[test]
    fn one_r_gate_excludes_bar_whose_period_ends_one_second_before_entry_at() {
        let entry_at = Utc::now();
        let position = long_position(entry_at);
        // Bar's period ends at entry_at - 1s, one second before entry_at —
        // must stay excluded.
        let history = VecDeque::from([candle(
            entry_at - Duration::minutes(15) - Duration::seconds(1),
            dec!(101.2),
            dec!(99.8),
        )]);
        let current = candle(entry_at + Duration::minutes(5), dec!(100.5), dec!(99.9));

        assert!(!FxNewV1::reached_one_r(&position, &history, &current));
    }

    fn base_time() -> chrono::DateTime<Utc> {
        use chrono::TimeZone;
        // Monday 00:00 UTC, aligned to a 15-minute bucket boundary.
        Utc.with_ymd_and_hms(2026, 1, 5, 0, 0, 0).unwrap()
    }

    fn price_event_with_exchange(
        pair: &str,
        exchange: Exchange,
        timeframe: &str,
        timestamp: chrono::DateTime<Utc>,
        ohlc: (Decimal, Decimal, Decimal, Decimal),
        volume: u64,
    ) -> PriceEvent {
        let (open, high, low, close) = ohlc;
        PriceEvent {
            pair: Pair::new(pair),
            exchange,
            candle: Candle {
                pair: Pair::new(pair),
                exchange,
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

    fn price_event(
        pair: &str,
        timeframe: &str,
        timestamp: chrono::DateTime<Utc>,
        ohlc: (Decimal, Decimal, Decimal, Decimal),
        volume: u64,
    ) -> PriceEvent {
        price_event_with_exchange(pair, Exchange::GmoFx, timeframe, timestamp, ohlc, volume)
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
    async fn accepts_rejects_oanda_events_for_a_pair_the_strategy_also_trades_on_gmo_fx() {
        // fx_new_v1 is wired to the gmo_fx account only; if `accepts` matched
        // on pair alone, OANDA USD_JPY ticks would mix into the same M5/M15/H1
        // history as GMO FX USD_JPY and corrupt the breakout/trend signals.
        let oanda_m5_bars = [
            (0_i64, (dec!(100), dec!(101), dec!(99), dec!(100.5))),
            (5, (dec!(100.5), dec!(103), dec!(100), dec!(102))),
            (10, (dec!(102), dec!(102.5), dec!(98), dec!(101))),
        ];
        let make_oanda_m5 = |minute: i64, ohlc: (Decimal, Decimal, Decimal, Decimal)| {
            price_event_with_exchange(
                "USD_JPY",
                Exchange::Oanda,
                "M5",
                base_time() + Duration::minutes(minute),
                ohlc,
                1,
            )
        };
        let oanda_h1 = price_event_with_exchange(
            "USD_JPY",
            Exchange::Oanda,
            "H1",
            base_time(),
            (dec!(100), dec!(100), dec!(100), dec!(100)),
            0,
        );

        // on_price path: a full M15 bucket (3 M5 bars) plus an H1 bar from
        // OANDA must leave every internal map untouched.
        let mut on_price_s = strategy();
        for (minute, ohlc) in oanda_m5_bars {
            assert!(
                on_price_s
                    .on_price(&make_oanda_m5(minute, ohlc))
                    .await
                    .is_none()
            );
        }
        assert!(on_price_s.on_price(&oanda_h1).await.is_none());
        assert!(
            on_price_s.m5_pending.is_empty(),
            "OANDA M5 bars must not be buffered by the GMO-FX-only strategy"
        );
        assert!(
            on_price_s.h1.is_empty(),
            "OANDA H1 bar must not be stored by the GMO-FX-only strategy"
        );
        assert!(
            on_price_s.m15.is_empty(),
            "OANDA M5 bars must not complete an M15 candle for the GMO-FX-only strategy"
        );

        // warmup path: same guarantee for the bulk-replay entry point.
        let mut warmup_events: Vec<PriceEvent> = oanda_m5_bars
            .iter()
            .map(|(minute, ohlc)| make_oanda_m5(*minute, *ohlc))
            .collect();
        warmup_events.push(oanda_h1.clone());
        let mut warmup_s = strategy();
        warmup_s.warmup(&warmup_events).await;
        assert!(
            warmup_s.m5_pending.is_empty(),
            "OANDA M5 bars must not be buffered during warmup"
        );
        assert!(
            warmup_s.h1.is_empty(),
            "OANDA H1 bar must not be stored during warmup"
        );
        assert!(
            warmup_s.m15.is_empty(),
            "OANDA M5 bars must not complete an M15 candle during warmup"
        );

        // Positive control: the identical bar sequence for GMO FX (the
        // strategy's own exchange) must update all three maps, proving the
        // OANDA assertions above exercise the exchange filter and not some
        // unrelated gap (e.g. a pair-matching bug).
        let mut gmo_s = strategy();
        let (first_minute, first_ohlc) = oanda_m5_bars[0];
        let gmo_m5_first = price_event(
            "USD_JPY",
            "M5",
            base_time() + Duration::minutes(first_minute),
            first_ohlc,
            1,
        );
        assert!(gmo_s.on_price(&gmo_m5_first).await.is_none());
        assert_eq!(
            gmo_s.m5_pending.get("USD_JPY").map(Vec::len),
            Some(1),
            "GMO FX M5 bar must be buffered"
        );

        let gmo_h1 = price_event(
            "USD_JPY",
            "H1",
            base_time(),
            (dec!(100), dec!(100), dec!(100), dec!(100)),
            0,
        );
        assert!(gmo_s.on_price(&gmo_h1).await.is_none());
        assert_eq!(
            gmo_s.h1.get("USD_JPY").map(VecDeque::len),
            Some(1),
            "GMO FX H1 bar must be stored"
        );

        for (minute, ohlc) in &oanda_m5_bars[1..] {
            let event = price_event(
                "USD_JPY",
                "M5",
                base_time() + Duration::minutes(*minute),
                *ohlc,
                1,
            );
            gmo_s.on_price(&event).await;
        }
        assert_eq!(
            gmo_s.m15.get("USD_JPY").map(VecDeque::len),
            Some(1),
            "GMO FX M5 bars must complete an M15 candle"
        );
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

    #[tokio::test]
    async fn on_price_rejects_zero_close_without_panicking() {
        let mut s = strategy();
        // H1 downtrend so the Short direction's breakout condition
        // (entry < channel_low) is trivially satisfied once entry = 0.
        push_h1_closes(&mut s, (1..=50).rev().map(Decimal::from));

        // Seed 50 calm M15 bars (uniform high/low/close, so every true
        // range is 0.2) so the M5-triggered bucket below becomes the 51st
        // bar and clears the `ENTRY_CHANNEL + ATR_BASELINE + ATR_PERIOD + 1`
        // (51) warm-up gate.
        let calm: VecDeque<Candle> = (1..=50)
            .map(|i| {
                candle(
                    base_time() - Duration::minutes(15 * (51 - i)),
                    dec!(100.1),
                    dec!(99.9),
                )
            })
            .collect();
        s.m15.insert("USD_JPY".to_string(), calm);

        // Three M5 bars whose aggregated M15 candle has an extreme true
        // range (high 1000 / low 1 against a calm 100 baseline), driving
        // the current ATR far above the historical baseline average and
        // satisfying the expansion gate, while its close is exactly 0 —
        // the case that panics on `atr * ATR_MULT / entry` before the fix.
        assert!(
            s.on_price(&m5(0, (dec!(100), dec!(1000), dec!(100), dec!(100)), 1))
                .await
                .is_none()
        );
        assert!(
            s.on_price(&m5(5, (dec!(100), dec!(100), dec!(1), dec!(100)), 1))
                .await
                .is_none()
        );

        let signal = s
            .on_price(&m5(10, (dec!(100), dec!(100), dec!(100), dec!(0)), 1))
            .await;

        assert!(
            signal.is_none(),
            "non-positive close must not produce a signal"
        );
    }

    #[tokio::test]
    async fn on_price_excludes_m5_bar_with_zero_open_from_aggregation() {
        let mut s = strategy();
        // Only the middle (second) M5 bar is corrupt (open = 0); the other
        // two are clean. The M15 aggregate's open/high/low/close come from
        // the first/third bars and the high/low across all three, so a
        // corrupt open on the *middle* bar alone would never surface in the
        // aggregated M15 candle's own OHLC — proving this must be caught
        // per-M5, before aggregation, not only on the aggregated result.
        assert!(
            s.on_price(&m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10))
                .await
                .is_none()
        );
        assert!(
            s.on_price(&m5(5, (dec!(0), dec!(101), dec!(99), dec!(100.2)), 10))
                .await
                .is_none()
        );
        assert!(
            s.on_price(&m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(100)), 10))
                .await
                .is_none()
        );

        assert_eq!(
            s.m5_pending.get("USD_JPY").map(Vec::len),
            Some(2),
            "the invalid middle M5 bar must not be buffered for aggregation"
        );
        assert!(
            s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
            "a bucket missing its invalid middle M5 bar must never complete into an M15 candle"
        );
    }

    #[tokio::test]
    async fn on_price_excludes_m5_bar_with_negative_close_from_aggregation() {
        let mut s = strategy();
        // Same as above but the middle bar's close (not open) is corrupt
        // (negative); the first/third bars and the resulting high/low stay
        // positive, so the aggregated M15 candle's own OHLC would again hide
        // the defect without a per-M5 check.
        assert!(
            s.on_price(&m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10))
                .await
                .is_none()
        );
        assert!(
            s.on_price(&m5(5, (dec!(100.5), dec!(101), dec!(99), dec!(-1)), 10))
                .await
                .is_none()
        );
        assert!(
            s.on_price(&m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(100)), 10))
                .await
                .is_none()
        );

        assert_eq!(
            s.m5_pending.get("USD_JPY").map(Vec::len),
            Some(2),
            "the invalid middle M5 bar must not be buffered for aggregation"
        );
        assert!(
            s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
            "a bucket missing its invalid middle M5 bar must never complete into an M15 candle"
        );
    }

    #[tokio::test]
    async fn warmup_excludes_m5_bar_with_zero_open_from_aggregation() {
        let mut s = strategy();
        let events = vec![
            m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10),
            m5(5, (dec!(0), dec!(101), dec!(99), dec!(100.2)), 10),
            m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(100)), 10),
        ];

        s.warmup(&events).await;

        assert_eq!(
            s.m5_pending.get("USD_JPY").map(Vec::len),
            Some(2),
            "the invalid middle M5 bar must not be buffered for aggregation during warmup"
        );
        assert!(
            s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
            "a bucket missing its invalid middle M5 bar must never complete into an M15 candle during warmup"
        );
    }

    #[tokio::test]
    async fn warmup_excludes_m5_bar_with_negative_close_from_aggregation() {
        let mut s = strategy();
        let events = vec![
            m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10),
            m5(5, (dec!(100.5), dec!(101), dec!(99), dec!(-1)), 10),
            m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(100)), 10),
        ];

        s.warmup(&events).await;

        assert_eq!(
            s.m5_pending.get("USD_JPY").map(Vec::len),
            Some(2),
            "the invalid middle M5 bar must not be buffered for aggregation during warmup"
        );
        assert!(
            s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
            "a bucket missing its invalid middle M5 bar must never complete into an M15 candle during warmup"
        );
    }

    #[tokio::test]
    async fn warmup_discards_m15_bucket_when_aggregated_close_is_zero() {
        let mut s = strategy();
        // The aggregated M15 candle's close comes from the *last* M5 bar in
        // the bucket, so only that bar needs the bad value.
        let events = vec![
            m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10),
            m5(5, (dec!(100.5), dec!(101), dec!(99), dec!(100.2)), 10),
            m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(0)), 10),
        ];

        s.warmup(&events).await;

        assert!(
            s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
            "an M15 bucket whose aggregated close is non-positive must not enter history"
        );
    }

    #[tokio::test]
    async fn warmup_discards_m15_bucket_when_aggregated_high_is_zero() {
        let mut s = strategy();
        // The aggregated M15 candle's high is the max across all three M5
        // bars, so every bar needs the bad value for the aggregate to
        // inherit it.
        let events = vec![
            m5(0, (dec!(100), dec!(0), dec!(99), dec!(100)), 10),
            m5(5, (dec!(100), dec!(0), dec!(99), dec!(100)), 10),
            m5(10, (dec!(100), dec!(0), dec!(99), dec!(100)), 10),
        ];

        s.warmup(&events).await;

        assert!(
            s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
            "an M15 bucket whose aggregated high is non-positive must not enter history"
        );
    }

    #[tokio::test]
    async fn warmup_discards_m15_bucket_when_aggregated_low_is_negative() {
        let mut s = strategy();
        // The aggregated M15 candle's low is the min across all three M5
        // bars, so every bar needs the bad value for the aggregate to
        // inherit it.
        let events = vec![
            m5(0, (dec!(100), dec!(101), dec!(-1), dec!(100)), 10),
            m5(5, (dec!(100), dec!(101), dec!(-1), dec!(100)), 10),
            m5(10, (dec!(100), dec!(101), dec!(-1), dec!(100)), 10),
        ];

        s.warmup(&events).await;

        assert!(
            s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
            "an M15 bucket whose aggregated low is negative must not enter history"
        );
    }

    #[tokio::test]
    async fn warmup_discards_h1_bar_with_zero_close() {
        let mut s = strategy();
        let event = price_event(
            "USD_JPY",
            "H1",
            base_time(),
            (dec!(100), dec!(101), dec!(99), dec!(0)),
            0,
        );

        s.warmup(&[event]).await;

        assert!(
            s.h1.get("USD_JPY").is_none_or(VecDeque::is_empty),
            "an H1 bar with a non-positive close must not enter history"
        );
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

    #[tokio::test]
    async fn on_open_positions_suppresses_exit_when_current_candle_low_is_non_positive() {
        let mut s = strategy();
        let entry_at = base_time();
        let position = long_position(entry_at);
        // Identical setup to `on_open_positions_exits_below_channel_once_one_r_reached`
        // (1R reached, close below the exit channel) except the current M5
        // candle's low is corrupt (non-positive); the exit must be
        // suppressed rather than computed from bad market data.
        s.m15.insert(
            "USD_JPY".to_string(),
            history_after_entry(entry_at, dec!(101)),
        );
        let event = price_event(
            "USD_JPY",
            "M5",
            entry_at + Duration::minutes(15 * 11),
            (dec!(99.8), dec!(100.2), dec!(0), dec!(99)),
            0,
        );

        let exits = s
            .on_open_positions(std::slice::from_ref(&position), &event)
            .await;

        assert!(
            exits.is_empty(),
            "a non-positive OHLC value on the current candle must suppress exit evaluation"
        );
    }

    #[tokio::test]
    async fn on_open_positions_uses_exit_channel_updated_by_latest_completed_bar_long() {
        let mut s = strategy();
        let entry_at = base_time();
        let position = long_position(entry_at);
        // 9 M15 bars. Bar 1 (oldest) carries the high that satisfies the 1R
        // gate (target 101). Bars 1-8 sit at a 99.5 low baseline; bar 9 (the
        // most recently *completed* bar) prints a new low of 98.0. The old
        // exit channel (period 8, include_current=false) is built from bars
        // 1-8 and never sees that 98.0 low, so its exit_low stays stale at
        // 99.5. The fixed channel (include_current=true) is built from bars
        // 2-9 and correctly drops exit_low to 98.0.
        let history: VecDeque<Candle> = (1..=9)
            .map(|i| {
                let (high, low) = match i {
                    1 => (dec!(101.2), dec!(99.5)),
                    9 => (dec!(100.5), dec!(98.0)),
                    _ => (dec!(100.5), dec!(99.5)),
                };
                candle(entry_at + Duration::minutes(15 * i), high, low)
            })
            .collect();
        s.m15.insert("USD_JPY".to_string(), history);
        // close 99.0 sits between the stale exit_low (99.5) and the updated
        // exit_low (98.0): the fixed channel must hold the position, the
        // stale one would exit here.
        let event = price_event(
            "USD_JPY",
            "M5",
            entry_at + Duration::minutes(15 * 10),
            (dec!(99.2), dec!(99.5), dec!(98.5), dec!(99.0)),
            0,
        );

        let exits = s
            .on_open_positions(std::slice::from_ref(&position), &event)
            .await;

        assert!(
            exits.is_empty(),
            "close 99.0 is above the updated exit_low 98.0; the stale \
             pre-fix channel (exit_low 99.5, ignoring the latest completed \
             bar) would have exited here"
        );
    }

    /// Mirrors the engine's M15-completing tick: the last history bar is the
    /// bucket that the current (third) M5 event just completed, so its
    /// high/low already contain that M5 candle. Bars 1-9 are older completed
    /// bars; bar 10 is the just-completed bucket (starts 10 minutes before the
    /// event).
    fn history_with_bucket_completed_by_event(
        entry_at: chrono::DateTime<Utc>,
        older_bar: impl Fn(i64) -> (Decimal, Decimal),
        completed_bucket: (Decimal, Decimal),
    ) -> (VecDeque<Candle>, chrono::DateTime<Utc>) {
        let mut history: VecDeque<Candle> = (1..=9)
            .map(|i| {
                let (high, low) = older_bar(i);
                candle(entry_at + Duration::minutes(15 * i), high, low)
            })
            .collect();
        let bucket_start = entry_at + Duration::minutes(15 * 10);
        history.push_back(candle(bucket_start, completed_bucket.0, completed_bucket.1));
        (history, bucket_start + Duration::minutes(10))
    }

    #[tokio::test]
    async fn on_open_positions_exits_long_on_tick_that_completes_m15_bar() {
        let mut s = strategy();
        let entry_at = base_time();
        let position = long_position(entry_at);
        // Bar 1 reaches 1R (101); bars 2-9 hold a 99.5 low. Bar 10 is the
        // bucket completed by this very M5 event and carries its low (98.8).
        let (history, event_ts) = history_with_bucket_completed_by_event(
            entry_at,
            |i| {
                if i == 1 {
                    (dec!(101.2), dec!(99.5))
                } else {
                    (dec!(100.5), dec!(99.5))
                }
            },
            (dec!(100.5), dec!(98.8)),
        );
        s.m15.insert("USD_JPY".to_string(), history);
        let event = price_event(
            "USD_JPY",
            "M5",
            event_ts,
            (dec!(99.2), dec!(99.4), dec!(98.8), dec!(99.0)),
            0,
        );

        let exits = s
            .on_open_positions(std::slice::from_ref(&position), &event)
            .await;

        assert_eq!(
            exits.len(),
            1,
            "close 99.0 is below the 8 prior completed bars' low (99.5); the \
             bucket containing the current M5 (low 98.8) must not widen the channel"
        );
    }

    #[tokio::test]
    async fn on_open_positions_exits_short_on_tick_that_completes_m15_bar() {
        let mut s = strategy();
        let entry_at = base_time();
        let position = short_position(entry_at);
        // Bar 1 reaches 1R (99); bars 2-9 hold a 100.5 high. Bar 10 is the
        // bucket completed by this very M5 event and carries its high (101.2).
        let (history, event_ts) = history_with_bucket_completed_by_event(
            entry_at,
            |i| {
                if i == 1 {
                    (dec!(100.5), dec!(98.8))
                } else {
                    (dec!(100.5), dec!(99.5))
                }
            },
            (dec!(101.2), dec!(99.5)),
        );
        s.m15.insert("USD_JPY".to_string(), history);
        let event = price_event(
            "USD_JPY",
            "M5",
            event_ts,
            (dec!(100.8), dec!(101.2), dec!(100.6), dec!(101.0)),
            0,
        );

        let exits = s
            .on_open_positions(std::slice::from_ref(&position), &event)
            .await;

        assert_eq!(
            exits.len(),
            1,
            "close 101.0 is above the 8 prior completed bars' high (100.5); the \
             bucket containing the current M5 (high 101.2) must not widen the channel"
        );
    }

    #[tokio::test]
    async fn on_open_positions_uses_exit_channel_updated_by_latest_completed_bar_short() {
        let mut s = strategy();
        let entry_at = base_time();
        let position = short_position(entry_at);
        // Symmetric to the long case: bar 1 carries the low that satisfies
        // the 1R gate (target 99), bars 1-8 sit at a 100.5 high baseline,
        // and bar 9 (most recently completed) prints a new high of 102.0.
        // The stale pre-fix channel (bars 1-8) never sees it, so exit_high
        // stays at 100.5; the fixed channel (bars 2-9) raises exit_high to
        // 102.0.
        let history: VecDeque<Candle> = (1..=9)
            .map(|i| {
                let (high, low) = match i {
                    1 => (dec!(100.5), dec!(98.8)),
                    9 => (dec!(102.0), dec!(99.5)),
                    _ => (dec!(100.5), dec!(99.5)),
                };
                candle(entry_at + Duration::minutes(15 * i), high, low)
            })
            .collect();
        s.m15.insert("USD_JPY".to_string(), history);
        // close 101.0 sits between the stale exit_high (100.5) and the
        // updated exit_high (102.0): the fixed channel must hold the
        // position, the stale one would exit here.
        let event = price_event(
            "USD_JPY",
            "M5",
            entry_at + Duration::minutes(15 * 10),
            (dec!(100.8), dec!(101.5), dec!(100.5), dec!(101.0)),
            0,
        );

        let exits = s
            .on_open_positions(std::slice::from_ref(&position), &event)
            .await;

        assert!(
            exits.is_empty(),
            "close 101.0 is below the updated exit_high 102.0; the stale \
             pre-fix channel (exit_high 100.5, ignoring the latest \
             completed bar) would have exited here"
        );
    }

    /// 50 calm M15 bars (high 100.1 / low 99.9 / close 100) ending just
    /// before `base_time()`. Reused by the end-to-end `on_price` entry
    /// tests below: once the 51st (breakout) bar is appended it clears the
    /// `ENTRY_CHANNEL + ATR_BASELINE + ATR_PERIOD + 1` warm-up gate, and
    /// because every calm-to-calm true range is exactly
    /// max(0.2, 0.1, 0.1) = 0.2, the ATR baseline average used by the
    /// expansion gate is always exactly 0.2 regardless of which breakout
    /// bar follows.
    fn calm_m15_history() -> VecDeque<Candle> {
        (1..=50)
            .map(|i| {
                candle(
                    base_time() - Duration::minutes(15 * (51 - i)),
                    dec!(100.1),
                    dec!(99.9),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn on_price_emits_long_signal_on_breakout_with_h1_uptrend_and_atr_expansion() {
        let mut s = strategy();
        // H1 uptrend: fast SMA20 > slow SMA50, so h1_direction == Some(Long).
        push_h1_closes(&mut s, (1..=50).map(Decimal::from));
        s.m15.insert("USD_JPY".to_string(), calm_m15_history());

        // Three M5 bars aggregate into the 51st M15 bar. Its close (101.8)
        // clears the calm channel high (100.1), and its wide high/low
        // (103 / 99.8) against the prior calm close (100) give it a true
        // range of 3.2, far above the 0.2 baseline average.
        assert!(
            s.on_price(&m5(0, (dec!(100), dec!(103), dec!(99.9), dec!(100.5)), 10))
                .await
                .is_none()
        );
        assert!(
            s.on_price(&m5(5, (dec!(100.5), dec!(103), dec!(99.8), dec!(101)), 10))
                .await
                .is_none()
        );
        let signal = s
            .on_price(&m5(10, (dec!(101), dec!(103), dec!(99.8), dec!(101.8)), 10))
            .await
            .expect("upward breakout + H1 uptrend + ATR expansion must emit a signal");

        assert_eq!(signal.direction, Direction::Long);
        // Wilder's smoothing folds the breakout bar's true range (3.2) into
        // the constant 0.2 ATR carried by the calm history:
        // (0.2 * 13 + 3.2) / 14. Asserting equality against this
        // independently-derived value (not just the sign/bound) confirms
        // stop_loss_pct is computed from the actual ATR, not a stale or
        // default one.
        let expected_atr = (dec!(0.2) * dec!(13) + dec!(3.2)) / dec!(14);
        let expected_stop_loss_pct = (expected_atr * ATR_MULT / dec!(101.8)).min(SL_CAP);
        assert!(signal.stop_loss_pct > Decimal::ZERO && signal.stop_loss_pct <= SL_CAP);
        assert_eq!(signal.stop_loss_pct, expected_stop_loss_pct);
        assert_eq!(
            signal.allocation_pct,
            FxNewV1::allocation_for(signal.stop_loss_pct)
        );
    }

    #[tokio::test]
    async fn on_price_emits_short_signal_on_breakdown_with_h1_downtrend_and_atr_expansion() {
        let mut s = strategy();
        // H1 downtrend: fast SMA20 < slow SMA50, so h1_direction == Some(Short).
        push_h1_closes(&mut s, (1..=50).rev().map(Decimal::from));
        s.m15.insert("USD_JPY".to_string(), calm_m15_history());

        // Symmetric to the long case: close (98.2) clears the calm channel
        // low (99.9) on the downside, and the wide high/low (100.2 / 96.8)
        // against the prior calm close (100) give a true range of 3.4.
        assert!(
            s.on_price(&m5(0, (dec!(100), dec!(100.2), dec!(97), dec!(99.5)), 10))
                .await
                .is_none()
        );
        assert!(
            s.on_price(&m5(5, (dec!(99.5), dec!(100.2), dec!(96.8), dec!(99)), 10))
                .await
                .is_none()
        );
        let signal = s
            .on_price(&m5(10, (dec!(99), dec!(100.2), dec!(96.8), dec!(98.2)), 10))
            .await
            .expect("downward breakout + H1 downtrend + ATR expansion must emit a signal");

        assert_eq!(signal.direction, Direction::Short);
        // (0.2 * 13 + 3.4) / 14, the same Wilder-smoothing fold as the long
        // case above but with this bar's true range (3.4).
        let expected_atr = (dec!(0.2) * dec!(13) + dec!(3.4)) / dec!(14);
        let expected_stop_loss_pct = (expected_atr * ATR_MULT / dec!(98.2)).min(SL_CAP);
        assert!(signal.stop_loss_pct > Decimal::ZERO && signal.stop_loss_pct <= SL_CAP);
        assert_eq!(signal.stop_loss_pct, expected_stop_loss_pct);
        assert_eq!(
            signal.allocation_pct,
            FxNewV1::allocation_for(signal.stop_loss_pct)
        );
    }

    #[tokio::test]
    async fn on_price_suppresses_signal_when_breakout_direction_disagrees_with_h1() {
        let mut s = strategy();
        // Same breakout bar as the long-signal test above, but H1 is now a
        // downtrend: h1_direction returns Short, whose breakout condition
        // (entry < channel_low) this upward breakout bar does not satisfy,
        // so no signal is produced despite the channel breakout and ATR
        // expansion both firing — direction agreement is a separate,
        // necessary gate.
        push_h1_closes(&mut s, (1..=50).rev().map(Decimal::from));
        s.m15.insert("USD_JPY".to_string(), calm_m15_history());

        assert!(
            s.on_price(&m5(0, (dec!(100), dec!(103), dec!(99.9), dec!(100.5)), 10))
                .await
                .is_none()
        );
        assert!(
            s.on_price(&m5(5, (dec!(100.5), dec!(103), dec!(99.8), dec!(101)), 10))
                .await
                .is_none()
        );
        let signal = s
            .on_price(&m5(10, (dec!(101), dec!(103), dec!(99.8), dec!(101.8)), 10))
            .await;

        assert!(
            signal.is_none(),
            "upward breakout with a disagreeing H1 downtrend must not emit a signal"
        );
    }

    #[tokio::test]
    async fn on_price_suppresses_signal_when_atr_does_not_expand() {
        let mut s = strategy();
        // H1 agrees with the breakout direction so the ATR gate is isolated
        // as the sole reason for the lack of signal (it is also checked
        // before h1_direction/breakout in on_price, so this would gate the
        // signal even if H1 disagreed too).
        push_h1_closes(&mut s, (1..=50).map(Decimal::from));
        s.m15.insert("USD_JPY".to_string(), calm_m15_history());

        // The breakout bar's close (100.15) still clears the calm channel
        // high (100.1), but its high/low (100.15 / 100.0) stay within 0.15
        // of the prior calm close (100), giving a true range of 0.15 —
        // below the 0.2 baseline average — so `atr <= atr_sum / atr_count`
        // rejects it.
        assert!(
            s.on_price(&m5(
                0,
                (dec!(100), dec!(100.1), dec!(100), dec!(100.05)),
                10
            ))
            .await
            .is_none()
        );
        assert!(
            s.on_price(&m5(
                5,
                (dec!(100.05), dec!(100.1), dec!(100), dec!(100.1)),
                10
            ))
            .await
            .is_none()
        );
        let signal = s
            .on_price(&m5(
                10,
                (dec!(100.1), dec!(100.15), dec!(100), dec!(100.15)),
                10,
            ))
            .await;

        assert!(
            signal.is_none(),
            "breakout close without ATR expansion must not emit a signal"
        );
    }
}
