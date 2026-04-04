use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{MacroUpdate, Strategy};
use auto_trader_core::types::{Direction, Pair, Signal};
use rust_decimal::Decimal;

pub struct TrendFollowV1 {
    name: String,
    ma_short_period: usize,
    ma_long_period: usize,
    rsi_threshold: Decimal,
    pairs: Vec<Pair>,
    price_history: std::collections::HashMap<String, Vec<Decimal>>,
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
            price_history: std::collections::HashMap::new(),
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
        history.push(event.candle.close);

        if history.len() < self.ma_long_period + 1 {
            return None;
        }

        let sma_short = auto_trader_market::indicators::sma(history, self.ma_short_period)?;
        let sma_long = auto_trader_market::indicators::sma(history, self.ma_long_period)?;
        let rsi = event.indicators.get("rsi_14")?;

        let prev_closes = &history[..history.len() - 1];
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
    use auto_trader_core::types::Candle;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    fn make_price_event(pair: &str, close: Decimal, indicators: HashMap<String, Decimal>) -> PriceEvent {
        PriceEvent {
            pair: Pair::new(pair),
            candle: Candle {
                pair: Pair::new(pair),
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
}
