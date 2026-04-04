use crate::event::PriceEvent;
use crate::types::Signal;

pub struct MacroUpdate {
    pub summary: String,
    pub adjustments: std::collections::HashMap<String, String>,
}

pub trait Strategy: Send + 'static {
    fn name(&self) -> &str;
    fn on_price(
        &mut self,
        event: &PriceEvent,
    ) -> impl std::future::Future<Output = Option<Signal>> + Send;
    fn on_macro_update(&mut self, update: &MacroUpdate);
}
