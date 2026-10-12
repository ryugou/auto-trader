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
        anyhow::bail!("unsupported operation: send_child_order")
    }

    async fn get_child_orders(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        anyhow::bail!("unsupported operation: get_child_orders")
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        anyhow::bail!("unsupported operation: get_executions")
    }

    async fn get_positions(&self, _product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        anyhow::bail!("unsupported operation: get_positions")
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        anyhow::bail!("unsupported operation: get_collateral")
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("unsupported operation: cancel_child_order")
    }

    async fn resolve_position_id(
        &self,
        _product_code: &str,
        _after: chrono::DateTime<chrono::Utc>,
        _expected_side: crate::bitflyer_private::Side,
        _expected_size: rust_decimal::Decimal,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}
