use auto_trader_core::event::{PriceEvent, SignalEvent};
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy};
use auto_trader_core::types::Position;
use std::collections::HashMap;
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

    /// Returns names of all registered strategies.
    pub fn registered_names(&self) -> Vec<&str> {
        self.slots.iter().map(|s| s.strategy.name()).collect()
    }

    /// Seed all strategies with historical events (oldest → newest) so their
    /// indicator state is ready before the first live event arrives.
    ///
    /// Note on "disabled": this iterates *registered* strategies, including
    /// those whose `mode` is `"disabled"` — those still receive macro updates
    /// and warmup so they are ready if re-enabled later. Strategies whose
    /// top-level config `enabled = false` are never registered with the
    /// engine in the first place (see `main.rs`), so they are not warmed up.
    pub async fn warmup(&mut self, events: &[PriceEvent]) {
        for slot in &mut self.slots {
            slot.strategy.warmup(events).await;
        }
    }

    /// Dispatch PriceEvent to all enabled strategies.
    /// 1-pair-1-position constraint is enforced at the executor level (main.rs),
    /// not here. The engine simply forwards all signals.
    pub async fn on_price(&mut self, event: &PriceEvent) {
        for slot in &mut self.slots {
            if slot.mode == "disabled" {
                continue;
            }
            if let Some(signal) = slot.strategy.on_price(event).await
                && let Err(e) = self
                    .signal_tx
                    .send(SignalEvent {
                        signal,
                        indicators: event.indicators.clone(),
                    })
                    .await
            {
                tracing::error!("signal channel closed, dropping signal: {e}");
            }
        }
    }

    /// Dispatch a price event AND give every enabled strategy a chance to
    /// inspect its open positions and emit dynamic exit signals (trailing
    /// stops, indicator reversals, time limits, …).
    ///
    /// Caller passes `open_positions_by_strategy`, a map keyed by strategy
    /// name. Strategies with no entry in the map are treated as having no
    /// open positions for this tick. Strategies in `disabled` mode are
    /// skipped entirely (entries and exits both).
    ///
    /// Returned ExitSignals are *not* pushed onto the entry signal
    /// channel — they have a different shape (close vs open) and a
    /// dedicated exit-executor task in `main.rs` consumes the returned
    /// vec via its own channel.
    pub async fn on_price_with_positions(
        &mut self,
        event: &PriceEvent,
        open_positions_by_strategy: &HashMap<String, Vec<Position>>,
    ) -> Vec<ExitSignal> {
        let mut all_exits: Vec<ExitSignal> = Vec::new();
        for slot in &mut self.slots {
            if slot.mode == "disabled" {
                continue;
            }
            // 1) New entry signal
            if let Some(signal) = slot.strategy.on_price(event).await
                && let Err(e) = self
                    .signal_tx
                    .send(SignalEvent {
                        signal,
                        indicators: event.indicators.clone(),
                    })
                    .await
            {
                tracing::error!("signal channel closed, dropping signal: {e}");
            }
            // 2) Dynamic exit signals against this strategy's open positions
            let positions = open_positions_by_strategy
                .get(slot.strategy.name())
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let exits = slot.strategy.on_open_positions(positions, event).await;
            all_exits.extend(exits);
        }
        all_exits
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
        // on_macro_update does NOT check mode — all strategies get macro context
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

        // Even disabled strategies receive macro updates (they just don't emit signals)
        assert_eq!(updates.lock().unwrap().len(), 1);
    }

    /// Dummy strategy that records warmup events for testing.
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
        // Strategies with mode="disabled" are still registered (e.g. for
        // future re-enable) and should receive warmup state so they don't
        // start cold when flipped on.
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
}
