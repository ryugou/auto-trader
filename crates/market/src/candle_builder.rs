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
    /// Most recently seen bid price — stamped onto the completed candle.
    last_best_bid: Option<Decimal>,
    /// Most recently seen ask price — stamped onto the completed candle.
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

    /// Process a tick. If this tick starts a new period, the previous period's
    /// candle is completed and returned before the new tick is recorded.
    ///
    /// `best_bid` / `best_ask` are forwarded from the exchange ticker.
    /// Pass `None` for data sources that do not provide bid/ask (e.g. OANDA).
    /// The most recently seen values are stored and stamped onto the completed
    /// candle so consumers can read the prevailing spread at close time.
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
                // New period detected — complete the previous candle first
                self.complete_current()
            } else {
                None
            };
        // Update last seen bid/ask on every tick (even if it starts a new period).
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

    /// Returns a completed candle if the given timestamp is past the current period.
    ///
    /// `best_bid` / `best_ask` are used only when called directly (e.g. from the
    /// timer path); on the tick-driven path `on_tick` already updates
    /// `last_best_bid / last_best_ask` before calling `complete_current`.
    pub fn try_complete(
        &mut self,
        now: DateTime<Utc>,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
    ) -> Option<Candle> {
        let ps = self.current_period_start?;
        let period_end = ps + chrono::Duration::seconds(self.period_secs);
        if now < period_end {
            return None;
        }
        // Apply any final bid/ask that arrived with this clock tick.
        if best_bid.is_some() {
            self.last_best_bid = best_bid;
        }
        if best_ask.is_some() {
            self.last_best_ask = best_ask;
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    #[test]
    fn builds_candle_from_ticks() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut builder = CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, "M1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();

        assert!(
            builder
                .on_tick(dec!(15000000), dec!(0.1), base, None, None)
                .is_none()
        );
        assert!(
            builder
                .on_tick(
                    dec!(15100000),
                    dec!(0.2),
                    base + chrono::Duration::seconds(10),
                    Some(dec!(15099000)),
                    Some(dec!(15101000)),
                )
                .is_none()
        );
        assert!(
            builder
                .on_tick(
                    dec!(14900000),
                    dec!(0.15),
                    base + chrono::Duration::seconds(30),
                    None,
                    None,
                )
                .is_none()
        );
        assert!(
            builder
                .on_tick(
                    dec!(15050000),
                    dec!(0.05),
                    base + chrono::Duration::seconds(50),
                    Some(dec!(15049000)),
                    Some(dec!(15051000)),
                )
                .is_none()
        );

        // Minute hasn't ended yet — no candle emitted
        assert!(
            builder
                .try_complete(base + chrono::Duration::seconds(50), None, None)
                .is_none()
        );

        // Minute ends via try_complete; last bid/ask from ticks above are carried over
        let candle = builder
            .try_complete(base + chrono::Duration::seconds(61), None, None)
            .unwrap();
        assert_eq!(candle.open, dec!(15000000));
        assert_eq!(candle.high, dec!(15100000));
        assert_eq!(candle.low, dec!(14900000));
        assert_eq!(candle.close, dec!(15050000));
        assert_eq!(candle.exchange, Exchange::BitflyerCfd);
        // bid/ask from last tick should be stamped onto the candle
        assert_eq!(candle.best_bid, Some(dec!(15049000)));
        assert_eq!(candle.best_ask, Some(dec!(15051000)));
    }

    #[test]
    fn empty_period_returns_none() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut builder = CandleBuilder::new(pair, Exchange::BitflyerCfd, "M1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();
        assert!(
            builder
                .try_complete(base + chrono::Duration::seconds(61), None, None)
                .is_none()
        );
    }

    #[test]
    fn period_boundary_completes_previous_candle() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut builder = CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, "M1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();

        assert!(
            builder
                .on_tick(dec!(100), dec!(10), base, None, None)
                .is_none()
        );
        // Tick in next period completes previous candle
        let candle = builder.on_tick(
            dec!(200),
            dec!(5),
            base + chrono::Duration::seconds(60),
            None,
            None,
        );
        assert!(
            candle.is_some(),
            "on_tick should return completed candle at period boundary"
        );
        let candle = candle.unwrap();
        assert_eq!(candle.open, dec!(100));
        assert_eq!(candle.close, dec!(100));
        // No bid/ask provided, so fields should be None
        assert_eq!(candle.best_bid, None);
        assert_eq!(candle.best_ask, None);

        // New period's candle is building
        let candle2 = builder
            .try_complete(base + chrono::Duration::seconds(121), None, None)
            .unwrap();
        assert_eq!(candle2.open, dec!(200));
        assert_eq!(candle2.close, dec!(200));
    }
}
