use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{MacroUpdate, Strategy};
use auto_trader_core::types::{Direction, Exchange, Pair, Signal};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};

pub struct CryptoTrendV1 {
    name: String,
    ma_short_period: usize,
    ma_long_period: usize,
    rsi_threshold: Decimal,
    pairs: Vec<Pair>,
    price_history: HashMap<String, VecDeque<Decimal>>,
}

impl CryptoTrendV1 {
    pub fn new(
        name: String,
        ma_short: usize,
        ma_long: usize,
        rsi_threshold: Decimal,
        pairs: Vec<Pair>,
    ) -> Self {
        Self {
            name,
            ma_short_period: ma_short,
            ma_long_period: ma_long,
            rsi_threshold,
            pairs,
            price_history: HashMap::new(),
        }
    }
}

#[async_trait::async_trait]
impl Strategy for CryptoTrendV1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        // Only process BitflyerCfd events
        if event.exchange != Exchange::BitflyerCfd {
            return None;
        }

        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }

        let key = event.pair.0.clone();
        let history = self.price_history.entry(key).or_default();
        history.push_back(event.candle.close);

        let max_len = self.ma_long_period + 2;
        while history.len() > max_len {
            history.pop_front();
        }

        let closes: Vec<Decimal> = history.iter().copied().collect();
        if closes.len() < self.ma_long_period + 1 {
            return None;
        }

        let sma_short = auto_trader_market::indicators::sma(&closes, self.ma_short_period)?;
        let sma_long = auto_trader_market::indicators::sma(&closes, self.ma_long_period)?;
        let rsi = event.indicators.get("rsi_14")?;

        let prev_closes = &closes[..closes.len() - 1];
        let prev_sma_short =
            auto_trader_market::indicators::sma(prev_closes, self.ma_short_period)?;
        let prev_sma_long =
            auto_trader_market::indicators::sma(prev_closes, self.ma_long_period)?;

        let golden_cross = prev_sma_short <= prev_sma_long && sma_short > sma_long;
        let death_cross = prev_sma_short >= prev_sma_long && sma_short < sma_long;

        let entry = event.candle.close;
        // SL/TP: 2%/4% of entry price (R:R = 1:2)
        let sl_distance = entry * Decimal::new(2, 2); // 2%
        let tp_distance = entry * Decimal::new(4, 2); // 4%

        if golden_cross && rsi < &self.rsi_threshold {
            Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                entry_price: entry,
                stop_loss: entry - sl_distance,
                take_profit: entry + tp_distance,
                confidence: 0.7,
                timestamp: event.timestamp,
            })
        } else if death_cross && rsi > &(Decimal::from(100) - self.rsi_threshold) {
            Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                entry_price: entry,
                stop_loss: entry + sl_distance,
                take_profit: entry - tp_distance,
                confidence: 0.7,
                timestamp: event.timestamp,
            })
        } else {
            None
        }
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {
        // Crypto strategy ignores macro updates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::{Candle, Exchange};
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    fn make_price_event(
        pair: &str,
        exchange: Exchange,
        close: Decimal,
        indicators: HashMap<String, Decimal>,
    ) -> PriceEvent {
        PriceEvent {
            pair: Pair::new(pair),
            exchange,
            candle: Candle {
                pair: Pair::new(pair),
                exchange,
                timeframe: "M5".to_string(),
                open: close,
                high: close,
                low: close,
                close,
                volume: Some(100),
                timestamp: Utc::now(),
            },
            indicators,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn no_signal_insufficient_data() {
        let mut strat = CryptoTrendV1::new(
            "crypto_test".to_string(),
            20,
            50,
            dec!(70),
            vec![Pair::new("BTC_JPY")],
        );
        let event = make_price_event("BTC_JPY", Exchange::BitflyerCfd, dec!(10000000), HashMap::new());
        assert!(strat.on_price(&event).await.is_none());
    }

    #[tokio::test]
    async fn ignores_fx_pair() {
        let mut strat = CryptoTrendV1::new(
            "crypto_test".to_string(),
            3,
            5,
            dec!(70),
            vec![Pair::new("BTC_JPY")],
        );
        // Oanda exchange event should be ignored even if pair matches
        let event = make_price_event("BTC_JPY", Exchange::Oanda, dec!(10000000), HashMap::new());
        assert!(strat.on_price(&event).await.is_none());
    }

    #[tokio::test]
    async fn golden_cross_long_signal() {
        let mut strat = CryptoTrendV1::new(
            "crypto_test".to_string(),
            3,
            5,
            dec!(70),
            vec![Pair::new("BTC_JPY")],
        );

        // Feed 6 flat prices to fill history
        for _ in 0..6 {
            let event = make_price_event("BTC_JPY", Exchange::BitflyerCfd, dec!(10000000), HashMap::new());
            assert!(strat.on_price(&event).await.is_none());
        }

        // Spike up to trigger golden cross
        let mut indicators = HashMap::new();
        indicators.insert("rsi_14".to_string(), dec!(50));
        let event = make_price_event("BTC_JPY", Exchange::BitflyerCfd, dec!(11000000), indicators);
        let signal = strat.on_price(&event).await;

        assert!(signal.is_some(), "should emit Long signal on golden cross");
        let signal = signal.unwrap();
        assert_eq!(signal.direction, Direction::Long);
        assert_eq!(signal.entry_price, dec!(11000000));
        // SL = entry - 2%, TP = entry + 4%
        assert_eq!(signal.stop_loss, dec!(10780000));
        assert_eq!(signal.take_profit, dec!(11440000));
    }

    #[tokio::test]
    async fn death_cross_short_signal() {
        let mut strat = CryptoTrendV1::new(
            "crypto_test".to_string(),
            3,
            5,
            dec!(70),
            vec![Pair::new("BTC_JPY")],
        );

        // Feed 6 flat prices
        for _ in 0..6 {
            let event = make_price_event("BTC_JPY", Exchange::BitflyerCfd, dec!(10000000), HashMap::new());
            assert!(strat.on_price(&event).await.is_none());
        }

        // Drop to trigger death cross
        let mut indicators = HashMap::new();
        indicators.insert("rsi_14".to_string(), dec!(50));
        let event = make_price_event("BTC_JPY", Exchange::BitflyerCfd, dec!(9000000), indicators);
        let signal = strat.on_price(&event).await;

        assert!(signal.is_some(), "should emit Short signal on death cross");
        let signal = signal.unwrap();
        assert_eq!(signal.direction, Direction::Short);
        assert_eq!(signal.entry_price, dec!(9000000));
        // SL = entry + 2%, TP = entry - 4%
        assert_eq!(signal.stop_loss, dec!(9180000));
        assert_eq!(signal.take_profit, dec!(8640000));
    }
}
