use auto_trader_core::types::{Candle, Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

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
        DateTime::from_timestamp(truncated, 0).unwrap()
    }

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
            if price > self.high.unwrap() {
                self.high = Some(price);
            }
            if price < self.low.unwrap() {
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

fn checked_period_end(start: DateTime<Utc>, period_secs: i64) -> Option<DateTime<Utc>> {
    start.checked_add_signed(chrono::Duration::try_seconds(period_secs)?)
}

#[cfg(test)]
mod tests;
