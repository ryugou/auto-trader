use std::collections::HashMap;

use auto_trader_core::event::{PriceEvent, SignalEvent};
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy};
use auto_trader_core::types::Position;
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

    pub fn registered_names(&self) -> Vec<&str> {
        self.slots.iter().map(|s| s.strategy.name()).collect()
    }

    pub async fn warmup(&mut self, events: &[PriceEvent]) {
        for slot in &mut self.slots {
            slot.strategy.warmup(events).await;
        }
    }

    async fn dispatch_signal(
        signal_tx: &mpsc::Sender<SignalEvent>,
        strategy: &mut dyn Strategy,
        event: &PriceEvent,
    ) {
        if let Some(signal) = strategy.on_price(event).await
            && let Err(e) = signal_tx
                .send(SignalEvent {
                    signal,
                    indicators: event.indicators.clone(),
                })
                .await
        {
            tracing::error!(error = %e, "failed to send signal: channel closed");
        }
    }

    pub async fn on_price(&mut self, event: &PriceEvent) {
        for slot in &mut self.slots {
            if slot.mode == "disabled" {
                continue;
            }
            Self::dispatch_signal(&self.signal_tx, slot.strategy.as_mut(), event).await;
        }
    }

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
            Self::dispatch_signal(&self.signal_tx, slot.strategy.as_mut(), event).await;
            let positions = open_positions_by_strategy
                .get(slot.strategy.name())
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let exits = slot.strategy.on_open_positions(positions, event).await;
            all_exits.extend(exits);
        }
        all_exits
    }

    pub fn on_macro_update(&mut self, update: &MacroUpdate) {
        for slot in &mut self.slots {
            slot.strategy.on_macro_update(update);
        }
    }
}

#[cfg(test)]
mod tests;
