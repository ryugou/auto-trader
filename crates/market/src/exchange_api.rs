use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};

pub fn weighted_avg_fills<I>(fills: I) -> anyhow::Result<(Decimal, Decimal, Decimal)>
where
    I: IntoIterator<Item = (Decimal, Decimal, Decimal)>,
{
    let (total_size, total_notional, total_fee) = fills.into_iter().fold(
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
        |(s, n, f), (price, size, fee)| (s + size, n + price * size, f + fee),
    );
    if total_size.is_zero() {
        anyhow::bail!("invalid fills: total size is zero");
    }
    Ok((total_notional / total_size, total_size, total_fee))
}

#[derive(Debug, Clone, PartialEq)]
pub enum StopOrderStatus {
    Active,
    Executed { price: Decimal, commission: Decimal },
    Gone,
}

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

    async fn resolve_position_id(
        &self,
        product_code: &str,
        after: chrono::DateTime<chrono::Utc>,
        expected_side: Side,
        expected_size: Decimal,
    ) -> anyhow::Result<Option<String>>;

    fn requires_close_position_id(&self) -> bool {
        false
    }

    async fn fetch_close_sfd(&self, _product_code: &str) -> anyhow::Result<Decimal> {
        Ok(Decimal::ZERO)
    }

    async fn place_stop_order(
        &self,
        _product_code: &str,
        _close_side: Side,
        _size: Decimal,
        _trigger_price: Decimal,
        _position_id: Option<&str>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("unsupported operation: place_stop_order")
    }

    async fn cancel_stop_order(
        &self,
        _product_code: &str,
        _stop_order_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("unsupported operation: cancel_stop_order")
    }

    async fn stop_order_status(
        &self,
        _product_code: &str,
        _stop_order_id: &str,
    ) -> anyhow::Result<StopOrderStatus> {
        anyhow::bail!("unsupported operation: stop_order_status")
    }
}

#[cfg(test)]
mod tests;
