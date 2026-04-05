use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{MacroUpdate, Strategy};
use auto_trader_core::types::{Direction, Pair, Signal};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};

pub struct TrendFollowV1 {
    name: String,
    ma_short_period: usize,
    ma_long_period: usize,
    rsi_threshold: Decimal,
    pairs: Vec<Pair>,
    price_history: HashMap<String, VecDeque<Decimal>>,
}

impl TrendFollowV1 {
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
impl Strategy for TrendFollowV1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }

        let key = event.pair.0.clone();
        let history = self.price_history.entry(key).or_default();
        history.push_back(event.candle.close);

        // Keep only what we need: ma_long_period + 2 for cross detection
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
        let prev_sma_short = auto_trader_market::indicators::sma(prev_closes, self.ma_short_period)?;
        let prev_sma_long = auto_trader_market::indicators::sma(prev_closes, self.ma_long_period)?;

        let golden_cross = prev_sma_short <= prev_sma_long && sma_short > sma_long;
        let death_cross = prev_sma_short >= prev_sma_long && sma_short < sma_long;

        if golden_cross && rsi < &self.rsi_threshold {
            let entry = event.candle.close;
            let pip_size = if entry > Decimal::from(10) {
                Decimal::new(1, 2) // JPY pairs: 0.01
            } else {
                Decimal::new(1, 4) // others: 0.0001
            };
            let sl_pips = pip_size * Decimal::from(50);
            let tp_pips = pip_size * Decimal::from(100);

            Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                entry_price: entry,
                stop_loss: entry - sl_pips,
                take_profit: entry + tp_pips,
                confidence: 0.7,
                timestamp: event.timestamp,
            })
        } else if death_cross && rsi > &(Decimal::from(100) - self.rsi_threshold) {
            let entry = event.candle.close;
            let pip_size = if entry > Decimal::from(10) {
                Decimal::new(1, 2)
            } else {
                Decimal::new(1, 4)
            };
            let sl_pips = pip_size * Decimal::from(50);
            let tp_pips = pip_size * Decimal::from(100);

            Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                entry_price: entry,
                stop_loss: entry + sl_pips,
                take_profit: entry - tp_pips,
                confidence: 0.7,
                timestamp: event.timestamp,
            })
        } else {
            None
        }
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {
        // Short-term rule-based strategy ignores macro updates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::{Candle, Exchange};
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    fn make_price_event(pair: &str, close: Decimal, indicators: HashMap<String, Decimal>) -> PriceEvent {
        PriceEvent {
            pair: Pair::new(pair),
            exchange: Exchange::Oanda,
            candle: Candle {
                pair: Pair::new(pair),
                exchange: Exchange::Oanda,
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
        let mut strat = TrendFollowV1::new(
            "test".to_string(), 20, 50, dec!(70), vec![Pair::new("USD_JPY")],
        );
        let event = make_price_event("USD_JPY", dec!(150), HashMap::new());
        assert!(strat.on_price(&event).await.is_none());
    }

    #[tokio::test]
    async fn ignores_untracked_pair() {
        let mut strat = TrendFollowV1::new(
            "test".to_string(), 20, 50, dec!(70), vec![Pair::new("USD_JPY")],
        );
        let event = make_price_event("EUR_USD", dec!(1.10), HashMap::new());
        assert!(strat.on_price(&event).await.is_none());
    }

    #[tokio::test]
    async fn golden_cross_with_custom_ma_periods() {
        // ma_short=3, ma_long=5 (non-default) to verify configurable periods work
        let mut strat = TrendFollowV1::new(
            "test".to_string(), 3, 5, dec!(70), vec![Pair::new("USD_JPY")],
        );

        // Feed 6 flat prices to fill history (need ma_long+1=6)
        for _ in 0..6 {
            let event = make_price_event("USD_JPY", dec!(150), HashMap::new());
            assert!(strat.on_price(&event).await.is_none());
        }

        // 7th price: spike up to trigger golden cross
        // prev: sma_3=150, sma_5=150 (equal)
        // curr: sma_3=(150+150+160)/3≈153.33, sma_5=(150+150+150+150+160)/5=152
        // golden_cross: prev_short<=prev_long && curr_short>curr_long = true
        let mut indicators = HashMap::new();
        indicators.insert("rsi_14".to_string(), dec!(50)); // RSI below threshold
        let event = make_price_event("USD_JPY", dec!(160), indicators);
        let signal = strat.on_price(&event).await;

        assert!(signal.is_some(), "should emit Long signal on golden cross with custom MA periods");
        let signal = signal.unwrap();
        assert_eq!(signal.direction, Direction::Long);
        assert_eq!(signal.strategy_name, "test");
    }

    #[tokio::test]
    async fn death_cross_with_custom_ma_periods() {
        let mut strat = TrendFollowV1::new(
            "test".to_string(), 3, 5, dec!(70), vec![Pair::new("USD_JPY")],
        );

        // Feed 6 flat prices
        for _ in 0..6 {
            let event = make_price_event("USD_JPY", dec!(150), HashMap::new());
            assert!(strat.on_price(&event).await.is_none());
        }

        // 7th price: drop to trigger death cross
        // prev: sma_3=150, sma_5=150 (equal)
        // curr: sma_3=(150+150+140)/3≈146.67, sma_5=(150+150+150+150+140)/5=148
        // death_cross: prev_short>=prev_long && curr_short<curr_long = true
        let mut indicators = HashMap::new();
        indicators.insert("rsi_14".to_string(), dec!(50)); // RSI above (100-70)=30
        let event = make_price_event("USD_JPY", dec!(140), indicators);
        let signal = strat.on_price(&event).await;

        assert!(signal.is_some(), "should emit Short signal on death cross with custom MA periods");
        let signal = signal.unwrap();
        assert_eq!(signal.direction, Direction::Short);
    }
}
