//! Trait abstraction over exchange-specific price feeds.
//!
//! Adding a new exchange's price feed means implementing this trait.
//! `main.rs` consumes the trait object via a per-Exchange registry,
//! mirroring the ExchangeApi abstraction on the order-placement side.

use crate::price_store::PriceStore;
use async_trait::async_trait;
use auto_trader_core::event::PriceEvent;
use std::sync::Arc;
use tokio::sync::mpsc;

#[async_trait]
pub trait MarketFeed: Send + Sync {
    /// Spawn-able long-running task. Implementations:
    /// - manage their own connection lifecycle (WS reconnect, REST retry etc)
    /// - update `price_store` with tick-level bid/ask data when available
    /// - emit `PriceEvent` on each confirmed candle to `price_tx`
    ///
    /// Returning `Err` stops this feed (main.rs logs it). Normal termination
    /// is when `price_tx` closes (channel dropped) — impl should exit cleanly.
    async fn run(
        self: Arc<Self>,
        price_store: Arc<PriceStore>,
        price_tx: mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()>;
}
