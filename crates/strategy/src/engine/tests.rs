use std::sync::{Arc, Mutex as StdMutex};

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::Signal;

use super::*;

struct MacroRecorder {
    name: String,
    updates: Arc<StdMutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Strategy for MacroRecorder {
    fn name(&self) -> &str {
        &self.name
    }
    async fn on_price(&mut self, _event: &PriceEvent) -> Option<Signal> {
        None
    }
    fn on_macro_update(&mut self, update: &MacroUpdate) {
        self.updates.lock().unwrap().push(update.summary.clone());
    }
}

#[test]
fn on_macro_update_broadcasts_to_all_strategies() {
    let (tx, _rx) = mpsc::channel::<SignalEvent>(16);
    let mut engine = StrategyEngine::new(tx);

    let updates_a = Arc::new(StdMutex::new(Vec::new()));
    let updates_b = Arc::new(StdMutex::new(Vec::new()));

    engine.add_strategy(
        Box::new(MacroRecorder {
            name: "a".to_string(),
            updates: updates_a.clone(),
        }),
        "paper".to_string(),
    );
    engine.add_strategy(
        Box::new(MacroRecorder {
            name: "b".to_string(),
            updates: updates_b.clone(),
        }),
        "paper".to_string(),
    );

    let update = MacroUpdate {
        summary: "USD weak on NFP miss".to_string(),
        adjustments: std::collections::HashMap::new(),
    };
    engine.on_macro_update(&update);

    assert_eq!(updates_a.lock().unwrap().len(), 1);
    assert_eq!(updates_a.lock().unwrap()[0], "USD weak on NFP miss");
    assert_eq!(updates_b.lock().unwrap().len(), 1);
    assert_eq!(updates_b.lock().unwrap()[0], "USD weak on NFP miss");
}

#[test]
fn on_macro_update_skips_nothing_even_if_disabled() {
    let (tx, _rx) = mpsc::channel::<SignalEvent>(16);
    let mut engine = StrategyEngine::new(tx);

    let updates = Arc::new(StdMutex::new(Vec::new()));
    engine.add_strategy(
        Box::new(MacroRecorder {
            name: "disabled_strat".to_string(),
            updates: updates.clone(),
        }),
        "disabled".to_string(),
    );

    let update = MacroUpdate {
        summary: "test".to_string(),
        adjustments: std::collections::HashMap::new(),
    };
    engine.on_macro_update(&update);

    assert_eq!(updates.lock().unwrap().len(), 1);
}

struct WarmupRecorder {
    name: String,
    warmups: Arc<StdMutex<Vec<usize>>>,
}

#[async_trait::async_trait]
impl Strategy for WarmupRecorder {
    fn name(&self) -> &str {
        &self.name
    }
    async fn on_price(&mut self, _event: &PriceEvent) -> Option<Signal> {
        None
    }
    fn on_macro_update(&mut self, _update: &MacroUpdate) {}
    async fn warmup(&mut self, events: &[PriceEvent]) {
        self.warmups.lock().unwrap().push(events.len());
    }
}

#[tokio::test]
async fn warmup_dispatches_to_disabled_strategies_too() {
    use auto_trader_core::types::{Candle, Exchange, Pair};
    use chrono::Utc;
    use rust_decimal_macros::dec;

    let (tx, _rx) = mpsc::channel::<SignalEvent>(16);
    let mut engine = StrategyEngine::new(tx);

    let enabled_log = Arc::new(StdMutex::new(Vec::<usize>::new()));
    let disabled_log = Arc::new(StdMutex::new(Vec::<usize>::new()));
    engine.add_strategy(
        Box::new(WarmupRecorder {
            name: "enabled".to_string(),
            warmups: enabled_log.clone(),
        }),
        "paper".to_string(),
    );
    engine.add_strategy(
        Box::new(WarmupRecorder {
            name: "disabled".to_string(),
            warmups: disabled_log.clone(),
        }),
        "disabled".to_string(),
    );

    let candle = Candle {
        pair: Pair::new("X"),
        exchange: Exchange::BitflyerCfd,
        timeframe: "M5".to_string(),
        open: dec!(1),
        high: dec!(1),
        low: dec!(1),
        close: dec!(1),
        volume: Some(0),
        best_bid: None,
        best_ask: None,
        timestamp: Utc::now(),
    };
    let events = vec![
        PriceEvent {
            pair: Pair::new("X"),
            exchange: Exchange::BitflyerCfd,
            timestamp: candle.timestamp,
            candle: candle.clone(),
            indicators: std::collections::HashMap::new(),
        },
        PriceEvent {
            pair: Pair::new("X"),
            exchange: Exchange::BitflyerCfd,
            timestamp: candle.timestamp,
            candle,
            indicators: std::collections::HashMap::new(),
        },
    ];
    engine.warmup(&events).await;

    assert_eq!(enabled_log.lock().unwrap().as_slice(), &[2usize]);
    assert_eq!(disabled_log.lock().unwrap().as_slice(), &[2usize]);
}
