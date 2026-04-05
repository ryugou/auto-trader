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

    /// Broadcast MacroUpdate to all strategies.
    pub fn on_macro_update(&mut self, update: &MacroUpdate) {
        for slot in &mut self.slots {
            slot.strategy.on_macro_update(update);
        }
    }
}
