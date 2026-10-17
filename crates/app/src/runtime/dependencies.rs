use std::collections::HashMap;
use std::sync::Arc;

use auto_trader::closer::CloseContext;
use auto_trader::price_store::PriceStore;
use auto_trader_core::event::TradeEvent;
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_core::types::Exchange;
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_notify::Notifier;
use rust_decimal::Decimal;
use tokio::sync::mpsc;

pub(crate) struct SharedDependencies {
    pub(crate) pool: sqlx::PgPool,
    pub(crate) exchange_apis: Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>,
    pub(crate) price_store: Arc<PriceStore>,
    pub(crate) notifier: Arc<Notifier>,
    pub(crate) position_sizer: Arc<PositionSizer>,
    pub(crate) liquidation_levels: Arc<HashMap<Exchange, Decimal>>,
    pub(crate) knowledge_store: Option<Arc<dyn KnowledgeStore>>,
}

impl SharedDependencies {
    pub(crate) fn close_context(&self, trade_tx: &mpsc::Sender<TradeEvent>) -> CloseContext {
        CloseContext {
            pool: self.pool.clone(),
            apis: self.exchange_apis.clone(),
            price_store: self.price_store.clone(),
            notifier: self.notifier.clone(),
            position_sizer: self.position_sizer.clone(),
            liquidation_levels: self.liquidation_levels.clone(),
            trade_tx: trade_tx.clone(),
        }
    }
}
