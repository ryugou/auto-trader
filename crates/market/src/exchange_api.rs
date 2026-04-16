//! Trait abstraction over exchange-specific Private API clients.
//!
//! Adding a new exchange means implementing this trait for a new client
//! struct. `Trader` and `main.rs` dispatch consume the trait object, so
//! strategy + DB + Signal layers are already exchange-agnostic.

use async_trait::async_trait;

// Re-use the existing request/response types from bitflyer_private for now.
// If another exchange needs a different shape, introduce neutral types in
// this module in a follow-up — for now the trait mirrors bitFlyer's shape
// since it's the only implementor.
use crate::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse,
};

#[async_trait]
pub trait ExchangeApi: Send + Sync {
    async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse>;

    async fn get_child_orders(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>>;

    async fn get_executions(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>>;

    async fn get_positions(&self, product_code: &str) -> anyhow::Result<Vec<ExchangePosition>>;

    async fn get_collateral(&self) -> anyhow::Result<Collateral>;

    async fn cancel_child_order(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<()>;
}
