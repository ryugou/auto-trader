use std::collections::HashMap;

use auto_trader_core::types::{Candle, Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

/// Builder of the candles of one pair and one timeframe from ticks.
pub struct CandleBuilder {
    pair: Pair,
    exchange: Exchange,
    timeframe: String,
    period_secs: i64,
    current_period_start: Option<DateTime<Utc>>,
    open: Option<Decimal>,
    high: Option<Decimal>,
    low: Option<Decimal>,
    close: Option<Decimal>,
    volume: Decimal,
    last_best_bid: Option<Decimal>,
    last_best_ask: Option<Decimal>,
}

impl CandleBuilder {
    /// Creates a builder for `pair` on `exchange` with candles of `timeframe`, which is `M1`, `M5` or `H1`.
    ///
    /// # Panics
    ///
    /// Panics if `timeframe` is not `M1`, `M5` or `H1`.
    pub fn new(pair: Pair, exchange: Exchange, timeframe: String) -> Self {
        let period_secs = match timeframe.as_str() {
            "M1" => 60,
            "M5" => 300,
            "H1" => 3600,
            other => panic!("unsupported timeframe: {other}"),
        };
        Self {
            pair,
            exchange,
            timeframe,
            period_secs,
            current_period_start: None,
            open: None,
            high: None,
            low: None,
            close: None,
            volume: Decimal::ZERO,
            last_best_bid: None,
            last_best_ask: None,
        }
    }

    fn period_start(&self, ts: DateTime<Utc>) -> DateTime<Utc> {
        let secs = ts.timestamp();
        let truncated = secs - (secs % self.period_secs);
        let Some(start) = DateTime::from_timestamp(truncated, 0) else {
            unreachable!(
                "truncating a timestamp towards its period boundary cannot leave DateTime<Utc>'s representable range"
            )
        };
        start
    }

    /// Records a tick in the candle of its period and returns the finished candle of the previous period when the tick falls in a different period.
    ///
    /// The returned candle carries the best bid and ask kept before this tick; when given, `best_bid` and `best_ask` replace the kept values.
    pub fn on_tick(
        &mut self,
        price: Decimal,
        size: Decimal,
        ts: DateTime<Utc>,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
    ) -> Option<Candle> {
        let ps = self.period_start(ts);
        let completed =
            if self.current_period_start.is_some() && self.current_period_start != Some(ps) {
                self.complete_current()
            } else {
                None
            };
        if best_bid.is_some() {
            self.last_best_bid = best_bid;
        }
        if best_ask.is_some() {
            self.last_best_ask = best_ask;
        }
        if self.current_period_start != Some(ps) {
            self.current_period_start = Some(ps);
            self.open = Some(price);
            self.high = Some(price);
            self.low = Some(price);
            self.close = Some(price);
            self.volume = size;
        } else {
            let Some(high) = self.high else {
                unreachable!("high is set whenever current_period_start is set")
            };
            if price > high {
                self.high = Some(price);
            }
            let Some(low) = self.low else {
                unreachable!("low is set whenever current_period_start is set")
            };
            if price < low {
                self.low = Some(price);
            }
            self.close = Some(price);
            self.volume += size;
        }
        completed
    }

    fn complete_current(&mut self) -> Option<Candle> {
        let ps = self.current_period_start?;
        let candle = Candle {
            pair: self.pair.clone(),
            exchange: self.exchange,
            timeframe: self.timeframe.clone(),
            open: self.open.take()?,
            high: self.high.take()?,
            low: self.low.take()?,
            close: self.close.take()?,
            volume: Some(self.volume.to_string().parse::<f64>().ok()?.round() as u64),
            best_bid: self.last_best_bid,
            best_ask: self.last_best_ask,
            timestamp: ps,
        };
        self.current_period_start = None;
        self.volume = Decimal::ZERO;
        Some(candle)
    }

    /// Finishes the open candle and returns it when its period has ended at `now`, and returns `None` when no candle is open or the period has not ended.
    ///
    /// When the period has ended, `best_bid` and `best_ask` replace the kept values, when given, before the candle is built.
    pub fn try_complete(
        &mut self,
        now: DateTime<Utc>,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
    ) -> Option<Candle> {
        let ps = self.current_period_start?;
        let period_end = checked_period_end(ps, self.period_secs)?;
        if now < period_end {
            return None;
        }
        if best_bid.is_some() {
            self.last_best_bid = best_bid;
        }
        if best_ask.is_some() {
            self.last_best_ask = best_ask;
        }
        self.complete_current()
    }
}

pub(crate) fn build_primary_and_h1_builders<'a>(
    pairs: impl IntoIterator<Item = &'a Pair>,
    exchange: Exchange,
    primary_timeframe: &str,
) -> (
    HashMap<String, CandleBuilder>,
    HashMap<String, CandleBuilder>,
) {
    let mut builders: HashMap<String, CandleBuilder> = HashMap::new();
    let mut h1_builders: HashMap<String, CandleBuilder> = HashMap::new();
    for pair in pairs {
        builders.insert(
            pair.0.clone(),
            CandleBuilder::new(pair.clone(), exchange, primary_timeframe.to_string()),
        );
        if primary_timeframe != "H1" {
            h1_builders.insert(
                pair.0.clone(),
                CandleBuilder::new(pair.clone(), exchange, "H1".to_string()),
            );
        }
    }
    (builders, h1_builders)
}

fn checked_period_end(start: DateTime<Utc>, period_secs: i64) -> Option<DateTime<Utc>> {
    start.checked_add_signed(chrono::Duration::try_seconds(period_secs)?)
}

#[cfg(test)]
mod tests;
