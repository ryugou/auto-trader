use crate::event::PriceEvent;
use crate::types::Signal;

#[derive(Clone)]
pub struct MacroUpdate {
    pub summary: String,
    pub adjustments: std::collections::HashMap<String, String>,
}

#[async_trait::async_trait]
pub trait Strategy: Send + 'static {
    fn name(&self) -> &str;
    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal>;
    fn on_macro_update(&mut self, update: &MacroUpdate);

    /// Seed internal state from historical PriceEvents (oldest → newest).
    /// Called once at startup before any live event so the strategy can build
    /// up indicator history from DB instead of waiting for real-time candles.
    /// Implementations must NOT emit signals here.
    async fn warmup(&mut self, _events: &[PriceEvent]) {}
}
