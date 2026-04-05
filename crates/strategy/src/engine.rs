use auto_trader_core::event::{PriceEvent, SignalEvent};
use auto_trader_core::strategy::{MacroUpdate, Strategy};
use tokio::sync::mpsc;

struct StrategySlot {
    strategy: Box<dyn Strategy>,
    mode: String,
}

pub struct StrategyEngine {
    slots: Vec<StrategySlot>,
    signal_tx: mpsc::Sender<SignalEvent>,
}

impl StrategyEngine {
    pub fn new(signal_tx: mpsc::Sender<SignalEvent>) -> Self {
        Self {
            slots: Vec::new(),
            signal_tx,
        }
    }

    pub fn add_strategy(&mut self, strategy: Box<dyn Strategy>, mode: String) {
        self.slots.push(StrategySlot { strategy, mode });
    }

    /// Dispatch PriceEvent to all enabled strategies.
    /// 1-pair-1-position constraint is enforced at the executor level (main.rs),
    /// not here. The engine simply forwards all signals.
    pub async fn on_price(&mut self, event: &PriceEvent) {
        for slot in &mut self.slots {
            if slot.mode == "disabled" {
                continue;
            }
            if let Some(signal) = slot.strategy.on_price(event).await {
                if let Err(e) = self.signal_tx.send(SignalEvent { signal }).await {
                    tracing::error!("signal channel closed, dropping signal: {e}");
                }
            }
        }
    }

    /// Broadcast MacroUpdate to all strategies, including disabled ones.
    /// Disabled strategies don't emit signals but still maintain macro context
    /// so they are ready if re-enabled without missing accumulated state.
    pub fn on_macro_update(&mut self, update: &MacroUpdate) {
        for slot in &mut self.slots {
            slot.strategy.on_macro_update(update);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::event::PriceEvent;
    use auto_trader_core::types::Signal;
    use std::sync::{Arc, Mutex as StdMutex};

    /// Dummy strategy that records macro updates for testing.
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
            Box::new(MacroRecorder { name: "a".to_string(), updates: updates_a.clone() }),
            "paper".to_string(),
        );
        engine.add_strategy(
            Box::new(MacroRecorder { name: "b".to_string(), updates: updates_b.clone() }),
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
        // on_macro_update does NOT check mode — all strategies get macro context
        let (tx, _rx) = mpsc::channel::<SignalEvent>(16);
        let mut engine = StrategyEngine::new(tx);

        let updates = Arc::new(StdMutex::new(Vec::new()));
        engine.add_strategy(
            Box::new(MacroRecorder { name: "disabled_strat".to_string(), updates: updates.clone() }),
            "disabled".to_string(),
        );

        let update = MacroUpdate {
            summary: "test".to_string(),
            adjustments: std::collections::HashMap::new(),
        };
        engine.on_macro_update(&update);

        // Even disabled strategies receive macro updates (they just don't emit signals)
        assert_eq!(updates.lock().unwrap().len(), 1);
    }
}
