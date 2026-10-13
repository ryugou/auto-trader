use anyhow::Context as _;
use async_trait::async_trait;
use reqwest::Method;
use rust_decimal::Decimal;

use crate::bitflyer_private::{
    ChildOrder, ChildOrderState, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};
use crate::exchange_api::{ExchangeApi, StopOrderStatus, weighted_avg_fills};

use super::http::GmoFxPrivateApi;
use super::types::{
    GmoAccountAssets, GmoExecution, GmoExecutionType, GmoListResponse, GmoOpenPosition, GmoOrder,
    GmoOrderResponseData, GmoSettlePosition, GmoStopCloseRequest, side_to_gmo,
};

fn gmo_status_to_state(status: &str) -> ChildOrderState {
    match status {
        "EXECUTED" => ChildOrderState::Completed,
        "CANCELED" => ChildOrderState::Canceled,
        "EXPIRED" => ChildOrderState::Expired,
        "REJECTED" => ChildOrderState::Rejected,
        _ => ChildOrderState::Active,
    }
}

#[async_trait]
impl ExchangeApi for GmoFxPrivateApi {
    async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        match req.close_position_id.clone() {
            None => self.post_open_order(req).await,
            Some(pid) => self.post_close_order(req, pid).await,
        }
    }

    async fn get_child_orders(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        let path = format!("/v1/orders?orderId={child_order_acceptance_id}");
        let data: GmoListResponse<GmoOrder> = self.signed_request(Method::GET, &path, None).await?;
        Ok(data
            .list
            .into_iter()
            .map(|o| ChildOrder {
                id: o.order_id,
                child_order_id: o.order_id.to_string(),
                product_code: o.symbol,
                side: o.side,
                child_order_type: o.execution_type,
                price: o.price,
                average_price: o.price,
                size: o.size,
                child_order_state: gmo_status_to_state(&o.status),
                expire_date: String::new(),
                child_order_date: o.timestamp,
                child_order_acceptance_id: o.order_id.to_string(),
                outstanding_size: o.size - o.executed_size,
                cancel_size: Decimal::ZERO,
                executed_size: o.executed_size,
                total_commission: Decimal::ZERO,
            })
            .collect())
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        let path = format!("/v1/executions?orderId={child_order_acceptance_id}");
        let data: GmoListResponse<GmoExecution> =
            self.signed_request(Method::GET, &path, None).await?;
        Ok(data
            .list
            .into_iter()
            .map(|e| Execution {
                id: e.execution_id,
                child_order_id: e.order_id.to_string(),
                side: e.side,
                price: e.price,
                size: e.size,
                commission: e.fee,
                exec_date: e.timestamp,
                child_order_acceptance_id: e.order_id.to_string(),
            })
            .collect())
    }

    async fn get_positions(&self, product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        let path = format!("/v1/openPositions?symbol={product_code}");
        let data: GmoListResponse<GmoOpenPosition> =
            self.signed_request(Method::GET, &path, None).await?;
        Ok(data
            .list
            .into_iter()
            .map(|p| ExchangePosition {
                product_code: p.symbol,
                side: p.side,
                price: p.price,
                size: p.size,
                commission: Decimal::ZERO,
                swap_point_accumulate: Decimal::ZERO,
                require_collateral: Decimal::ZERO,
                open_date: p.timestamp,
                leverage: Decimal::ZERO,
                pnl: Decimal::ZERO,
                sfd: Decimal::ZERO,
            })
            .collect())
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        let data: GmoAccountAssets = self
            .signed_request(Method::GET, "/v1/account/assets", None)
            .await?;
        Ok(Collateral {
            collateral: data.balance,
            open_position_pnl: data.position_loss_gain,
            require_collateral: data.margin,
            keep_rate: data.margin_ratio,
        })
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        let order_id: u64 = child_order_acceptance_id
            .parse()
            .context("invalid child_order_acceptance_id: not a u64")?;
        let body = serde_json::json!({ "orderId": order_id });
        let _: serde_json::Value = self
            .signed_request(Method::POST, "/v1/cancelOrder", Some(&body))
            .await?;
        Ok(())
    }

    async fn resolve_position_id(
        &self,
        product_code: &str,
        after: chrono::DateTime<chrono::Utc>,
        expected_side: Side,
        expected_size: Decimal,
    ) -> anyhow::Result<Option<String>> {
        let path = format!("/v1/openPositions?symbol={product_code}");
        let data: GmoListResponse<GmoOpenPosition> =
            self.signed_request(Method::GET, &path, None).await?;
        let expected_side_str = match expected_side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };
        let mut newest: Option<(chrono::DateTime<chrono::Utc>, u64)> = None;
        for p in data.list {
            if p.side != expected_side_str {
                continue;
            }
            if p.size != expected_size {
                continue;
            }
            let Ok(ts) = p.timestamp.parse::<chrono::DateTime<chrono::Utc>>() else {
                continue;
            };
            if ts < after {
                continue;
            }
            match newest {
                None => newest = Some((ts, p.position_id)),
                Some((cur_ts, _)) if ts > cur_ts => newest = Some((ts, p.position_id)),
                _ => {}
            }
        }
        Ok(newest.map(|(_, pid)| pid.to_string()))
    }

    fn requires_close_position_id(&self) -> bool {
        true
    }

    async fn place_stop_order(
        &self,
        product_code: &str,
        close_side: Side,
        size: Decimal,
        trigger_price: Decimal,
        position_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let position_id_str = position_id
            .ok_or_else(|| anyhow::anyhow!("invalid position_id: required for a netting close"))?;
        let position_id: u64 = position_id_str
            .parse()
            .context("invalid position_id: not a u64")?;
        let body = GmoStopCloseRequest {
            symbol: product_code.to_string(),
            side: side_to_gmo(close_side),
            execution_type: GmoExecutionType::Stop,
            price: trigger_price.to_string(),
            settle_position: vec![GmoSettlePosition {
                position_id,
                size: size.to_string(),
            }],
        };
        let body_val = serde_json::to_value(&body)?;
        let data: GmoOrderResponseData = self
            .signed_request(Method::POST, "/v1/closeOrder", Some(&body_val))
            .await?;
        Ok(data.root_order_id.to_string())
    }

    async fn cancel_stop_order(
        &self,
        _product_code: &str,
        stop_order_id: &str,
    ) -> anyhow::Result<()> {
        let order_id: u64 = stop_order_id
            .parse()
            .context("invalid stop_order_id: not a u64")?;
        let body = serde_json::json!({ "orderId": order_id });
        let _: serde_json::Value = self
            .signed_request(Method::POST, "/v1/cancelOrder", Some(&body))
            .await?;
        Ok(())
    }

    async fn stop_order_status(
        &self,
        _product_code: &str,
        stop_order_id: &str,
    ) -> anyhow::Result<StopOrderStatus> {
        let path = format!("/v1/orders?orderId={stop_order_id}");
        let data: GmoListResponse<GmoOrder> = self.signed_request(Method::GET, &path, None).await?;
        let Some(order) = data.list.into_iter().next() else {
            return Ok(StopOrderStatus::Gone);
        };
        match order.status.as_str() {
            "EXECUTED" => {
                let epath = format!("/v1/executions?orderId={stop_order_id}");
                let execs: GmoListResponse<GmoExecution> =
                    self.signed_request(Method::GET, &epath, None).await?;
                let (price, commission) =
                    match weighted_avg_fills(execs.list.iter().map(|e| (e.price, e.size, e.fee))) {
                        Ok((avg_price, _total_size, total_fee)) => (avg_price, total_fee),
                        Err(_) => (order.price, execs.list.iter().map(|e| e.fee).sum()),
                    };
                Ok(StopOrderStatus::Executed { price, commission })
            }
            "CANCELED" | "EXPIRED" | "REJECTED" => Ok(StopOrderStatus::Gone),
            _ => Ok(StopOrderStatus::Active),
        }
    }
}

#[cfg(test)]
mod tests;
