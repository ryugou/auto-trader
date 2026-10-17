use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};

/// Returns the volume-weighted average price, the total size and the total fee of `fills`, each given as a price, a size and a fee.
///
/// # Errors
///
/// - Returns an error if the total size of `fills` is zero, which includes an empty `fills`.
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

/// Status of a stop order on the exchange.
#[derive(Debug, Clone, PartialEq)]
pub enum StopOrderStatus {
    /// The order waits for its trigger price.
    Active,
    /// The order was triggered and filled.
    Executed {
        /// Fill price of the order.
        price: Decimal,
        /// Commission paid for the fill.
        commission: Decimal,
    },
    /// The order was canceled, expired, rejected or not found.
    Gone,
}

/// Private API of an exchange for orders, executions, positions, collateral and stop orders.
#[async_trait]
pub trait ExchangeApi: Send + Sync {
    /// Sends a child order and returns the response of the exchange with its acceptance ID.
    async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse>;

    /// Returns the child orders of `product_code` that have `child_order_acceptance_id`.
    async fn get_child_orders(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>>;

    /// Returns the executions of `product_code` that belong to the child order `child_order_acceptance_id`.
    async fn get_executions(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>>;

    /// Returns the open positions of `product_code`.
    async fn get_positions(&self, product_code: &str) -> anyhow::Result<Vec<ExchangePosition>>;

    /// Returns the collateral of the account.
    async fn get_collateral(&self) -> anyhow::Result<Collateral>;

    /// Cancels the child order `child_order_acceptance_id` of `product_code`.
    async fn cancel_child_order(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<()>;

    /// Returns the identifier of the newest open position of `product_code` on `expected_side` with `expected_size` that was opened at or after `after`, or `None` when the exchange finds none or does not look positions up.
    async fn resolve_position_id(
        &self,
        product_code: &str,
        after: chrono::DateTime<chrono::Utc>,
        expected_side: Side,
        expected_size: Decimal,
    ) -> anyhow::Result<Option<String>>;

    /// Returns whether a close order must name the position that it closes.
    fn requires_close_position_id(&self) -> bool {
        false
    }

    /// Returns the SFD of closing a position of `_product_code`, which is zero unless the exchange gives it.
    async fn fetch_close_sfd(&self, _product_code: &str) -> anyhow::Result<Decimal> {
        Ok(Decimal::ZERO)
    }

    /// Places a stop order and returns its identifier; it fails as unsupported unless the exchange overrides it.
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

    /// Cancels a stop order; it fails as unsupported unless the exchange overrides it.
    async fn cancel_stop_order(
        &self,
        _product_code: &str,
        _stop_order_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("unsupported operation: cancel_stop_order")
    }

    /// Returns the status of a stop order; it fails as unsupported unless the exchange overrides it.
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
