use std::sync::Arc;

use async_trait::async_trait;
use auto_trader_core::event::PriceEvent;
use tokio::sync::mpsc;

use crate::price_store::PriceStore;

/// Feed of prices.
#[async_trait]
pub trait MarketFeed: Send + Sync {
    /// Runs the feed and sends its price events to `price_tx` until the feed fails or `price_tx` is closed.
    async fn run(
        self: Box<Self>,
        price_store: Arc<PriceStore>,
        price_tx: mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()>;
}
