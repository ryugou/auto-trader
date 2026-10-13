use rust_decimal::Decimal;

use crate::exchange_api::{ExchangeApi, StopOrderStatus};

use super::http::BitflyerPrivateApi;
use super::types::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};

#[async_trait::async_trait]
impl ExchangeApi for BitflyerPrivateApi {
    async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        self.send_child_order(req)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn get_child_orders(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        self.get_child_orders(product_code, child_order_acceptance_id)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn get_executions(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        self.get_executions(product_code, child_order_acceptance_id)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn get_positions(&self, product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        self.get_positions(product_code)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        self.get_collateral().await.map_err(anyhow::Error::from)
    }

    async fn cancel_child_order(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        self.cancel_child_order(product_code, child_order_acceptance_id)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn resolve_position_id(
        &self,
        _product_code: &str,
        _after: chrono::DateTime<chrono::Utc>,
        _expected_side: Side,
        _expected_size: rust_decimal::Decimal,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    async fn fetch_close_sfd(&self, product_code: &str) -> anyhow::Result<Decimal> {
        self.fetch_close_sfd(product_code)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn place_stop_order(
        &self,
        product_code: &str,
        close_side: Side,
        size: Decimal,
        trigger_price: Decimal,
        _position_id: Option<&str>,
    ) -> anyhow::Result<String> {
        self.send_parent_order_stop(product_code, close_side, size, trigger_price)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn cancel_stop_order(
        &self,
        product_code: &str,
        stop_order_id: &str,
    ) -> anyhow::Result<()> {
        self.cancel_parent_order(product_code, stop_order_id)
            .await
            .map_err(anyhow::Error::from)
    }

    async fn stop_order_status(
        &self,
        product_code: &str,
        stop_order_id: &str,
    ) -> anyhow::Result<StopOrderStatus> {
        BitflyerPrivateApi::stop_order_status(self, product_code, stop_order_id)
            .await
            .map_err(anyhow::Error::from)
    }
}

#[cfg(test)]
mod tests;
