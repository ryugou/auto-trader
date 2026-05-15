//! Stub [`ExchangeApi`] that returns an error on every call.
//!
//! Used for paper/dry_run accounts whose exchange has no Private API
//! implementation yet (e.g. GMO Coin FX before account opening).
//! [`crate::unified_trader::UnifiedTrader`] never calls API methods when
//! `dry_run = true` — fills are sourced from the PriceStore instead — so
//! these errors should never fire in practice.

use async_trait::async_trait;

use crate::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse,
};
use crate::exchange_api::ExchangeApi;

pub struct NullExchangeApi;

#[async_trait]
impl ExchangeApi for NullExchangeApi {
    async fn send_child_order(
        &self,
        _req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        anyhow::bail!(
            "NullExchangeApi: send_child_order called on stub (dry_run account has no real API)"
        )
    }

    async fn get_child_orders(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        anyhow::bail!("NullExchangeApi: get_child_orders called on stub")
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        anyhow::bail!("NullExchangeApi: get_executions called on stub")
    }

    async fn get_positions(&self, _product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        anyhow::bail!("NullExchangeApi: get_positions called on stub")
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        anyhow::bail!("NullExchangeApi: get_collateral called on stub")
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("NullExchangeApi: cancel_child_order called on stub")
    }

    async fn resolve_position_id(
        &self,
        _product_code: &str,
        _after: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}
