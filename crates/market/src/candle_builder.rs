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
        }
    }

    fn period_start(&self, ts: DateTime<Utc>) -> DateTime<Utc> {
        let secs = ts.timestamp();
        let truncated = secs - (secs % self.period_secs);
        DateTime::from_timestamp(truncated, 0).unwrap()
    }

    pub fn on_tick(&mut self, price: Decimal, size: Decimal, ts: DateTime<Utc>) {
        let ps = self.period_start(ts);
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
    }

    /// Returns a completed candle if the given timestamp is past the current period.
    pub fn try_complete(&mut self, now: DateTime<Utc>) -> Option<Candle> {
        let ps = self.current_period_start?;
        let period_end = ps + chrono::Duration::seconds(self.period_secs);
        if now < period_end {
            return None;
        }
        let candle = Candle {
            pair: self.pair.clone(),
            exchange: self.exchange,
            timeframe: self.timeframe.clone(),
            open: self.open.take()?,
            high: self.high.take()?,
            low: self.low.take()?,
            close: self.close.take()?,
            volume: Some(
                self.volume
                    .to_string()
                    .parse::<f64>()
                    .ok()?
                    .round() as u64,
            ),
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
        let mut builder =
            CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, "M1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();

        builder.on_tick(dec!(15000000), dec!(0.1), base);
        builder.on_tick(
            dec!(15100000),
            dec!(0.2),
            base + chrono::Duration::seconds(10),
        );
        builder.on_tick(
            dec!(14900000),
            dec!(0.15),
            base + chrono::Duration::seconds(30),
        );
        builder.on_tick(
            dec!(15050000),
            dec!(0.05),
            base + chrono::Duration::seconds(50),
        );

        // Minute hasn't ended yet — no candle emitted
        assert!(builder
            .try_complete(base + chrono::Duration::seconds(50))
            .is_none());

        // Minute ends
        let candle = builder
            .try_complete(base + chrono::Duration::seconds(61))
            .unwrap();
        assert_eq!(candle.open, dec!(15000000));
        assert_eq!(candle.high, dec!(15100000));
        assert_eq!(candle.low, dec!(14900000));
        assert_eq!(candle.close, dec!(15050000));
        // 0.1 + 0.2 + 0.15 + 0.05 = 0.5 → rounds to 1 as u64
        assert_eq!(candle.volume, Some(1));
        assert_eq!(candle.exchange, Exchange::BitflyerCfd);
        assert_eq!(candle.pair, Pair::new("FX_BTC_JPY"));
        assert_eq!(candle.timeframe, "M1");
    }

    #[test]
    fn empty_period_returns_none() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut builder = CandleBuilder::new(pair, Exchange::BitflyerCfd, "M1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();
        assert!(builder
            .try_complete(base + chrono::Duration::seconds(61))
            .is_none());
    }

    #[test]
    fn period_boundary_resets_builder() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut builder =
            CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, "M1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();

        builder.on_tick(dec!(100), dec!(10), base);
        // Tick in next period resets state
        builder.on_tick(dec!(200), dec!(5), base + chrono::Duration::seconds(60));

        let candle = builder
            .try_complete(base + chrono::Duration::seconds(121))
            .unwrap();
        assert_eq!(candle.open, dec!(200));
        assert_eq!(candle.high, dec!(200));
        assert_eq!(candle.low, dec!(200));
        assert_eq!(candle.close, dec!(200));
    }
}
