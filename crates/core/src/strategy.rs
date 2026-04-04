use crate::event::PriceEvent;
use crate::types::Signal;

pub struct MacroUpdate {
    pub summary: String,
    pub adjustments: std::collections::HashMap<String, String>,
}

#[async_trait::async_trait]
pub trait Strategy: Send + 'static {
    fn name(&self) -> &str;
    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal>;
    fn on_macro_update(&mut self, update: &MacroUpdate);
}
