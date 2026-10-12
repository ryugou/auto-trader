use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use urlencoding::encode as url_encode;

use crate::exchange_api::{StopOrderStatus, weighted_avg_fills};

use super::http::{BitflyerPrivateApi, truncate_body};
use super::types::{
    BitflyerApiError, ChildOrder, Execution, SendChildOrderRequest, SendChildOrderResponse, Side,
};

#[derive(Debug, Serialize)]
struct ParentOrderParameter {
    product_code: String,
    condition_type: &'static str,
    side: Side,
    size: Decimal,
    trigger_price: Decimal,
}

#[derive(Debug, Serialize)]
struct SendParentOrderRequest {
    order_method: &'static str,
    parameters: Vec<ParentOrderParameter>,
}

#[derive(Debug, Deserialize)]
struct SendParentOrderResponse {
    parent_order_acceptance_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
enum ParentOrderState {
    Active,
    Completed,
    Canceled,
    Expired,
    Rejected,
}

#[derive(Debug, Deserialize)]
struct GetParentOrderResponse {
    parent_order_id: String,
    #[serde(default)]
    parent_order_state: Option<ParentOrderState>,
}

impl BitflyerPrivateApi {
    pub async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> Result<SendChildOrderResponse, BitflyerApiError> {
        let body = serde_json::to_string(&req).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!("failed to serialize request body: {e}"))
        })?;
        let text = self.request("POST", "/v1/me/sendchildorder", &body).await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    pub async fn get_child_orders(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> Result<Vec<ChildOrder>, BitflyerApiError> {
        let path = format!(
            "/v1/me/getchildorders?product_code={}&child_order_acceptance_id={}",
            url_encode(product_code),
            url_encode(child_order_acceptance_id),
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    pub async fn get_executions(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> Result<Vec<Execution>, BitflyerApiError> {
        let path = format!(
            "/v1/me/getexecutions?product_code={}&child_order_acceptance_id={}",
            url_encode(product_code),
            url_encode(child_order_acceptance_id),
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    pub async fn cancel_child_order(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> Result<(), BitflyerApiError> {
        #[derive(Serialize)]
        struct CancelRequest<'a> {
            product_code: &'a str,
            child_order_acceptance_id: &'a str,
        }
        let body = serde_json::to_string(&CancelRequest {
            product_code,
            child_order_acceptance_id,
        })
        .map_err(|e| {
            BitflyerApiError::InvalidResponse(format!("failed to serialize request body: {e}"))
        })?;
        let _ = self
            .request("POST", "/v1/me/cancelchildorder", &body)
            .await?;
        Ok(())
    }

    pub async fn send_parent_order_stop(
        &self,
        product_code: &str,
        close_side: Side,
        size: Decimal,
        trigger_price: Decimal,
    ) -> Result<String, BitflyerApiError> {
        let req = SendParentOrderRequest {
            order_method: "SIMPLE",
            parameters: vec![ParentOrderParameter {
                product_code: product_code.to_string(),
                condition_type: "STOP",
                side: close_side,
                size,
                trigger_price,
            }],
        };
        let body = serde_json::to_string(&req).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!("failed to serialize request body: {e}"))
        })?;
        let text = self
            .request("POST", "/v1/me/sendparentorder", &body)
            .await?;
        let resp: SendParentOrderResponse = serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })?;
        Ok(resp.parent_order_acceptance_id)
    }

    pub async fn cancel_parent_order(
        &self,
        product_code: &str,
        parent_order_acceptance_id: &str,
    ) -> Result<(), BitflyerApiError> {
        #[derive(Serialize)]
        struct CancelParentRequest<'a> {
            product_code: &'a str,
            parent_order_acceptance_id: &'a str,
        }
        let body = serde_json::to_string(&CancelParentRequest {
            product_code,
            parent_order_acceptance_id,
        })
        .map_err(|e| {
            BitflyerApiError::InvalidResponse(format!("failed to serialize request body: {e}"))
        })?;
        let _ = self
            .request("POST", "/v1/me/cancelparentorder", &body)
            .await?;
        Ok(())
    }

    async fn get_parent_order(
        &self,
        parent_order_acceptance_id: &str,
    ) -> Result<GetParentOrderResponse, BitflyerApiError> {
        let path = format!(
            "/v1/me/getparentorder?parent_order_acceptance_id={}",
            url_encode(parent_order_acceptance_id),
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    async fn get_child_orders_by_parent(
        &self,
        product_code: &str,
        parent_order_id: &str,
    ) -> Result<Vec<ChildOrder>, BitflyerApiError> {
        let path = format!(
            "/v1/me/getchildorders?product_code={}&parent_order_id={}",
            url_encode(product_code),
            url_encode(parent_order_id),
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    pub async fn stop_order_status(
        &self,
        product_code: &str,
        parent_order_acceptance_id: &str,
    ) -> Result<StopOrderStatus, BitflyerApiError> {
        let parent = match self.get_parent_order(parent_order_acceptance_id).await {
            Ok(p) => p,
            Err(BitflyerApiError::OrderNotFound(_)) => return Ok(StopOrderStatus::Gone),
            Err(e) => return Err(e),
        };

        match parent.parent_order_state {
            Some(ParentOrderState::Active) => Ok(StopOrderStatus::Active),
            Some(ParentOrderState::Canceled)
            | Some(ParentOrderState::Expired)
            | Some(ParentOrderState::Rejected) => Ok(StopOrderStatus::Gone),
            Some(ParentOrderState::Completed) | None => {
                let children = self
                    .get_child_orders_by_parent(product_code, &parent.parent_order_id)
                    .await?;
                let executed_child = children.iter().find(|c| c.executed_size > Decimal::ZERO);
                match executed_child {
                    Some(child) => {
                        let execs = self
                            .get_executions(product_code, &child.child_order_acceptance_id)
                            .await?;
                        let (price, commission) = match weighted_avg_fills(
                            execs.iter().map(|e| (e.price, e.size, e.commission)),
                        ) {
                            Ok((p, _size, comm)) => (p, comm),
                            Err(_) if child.average_price > Decimal::ZERO => {
                                (child.average_price, Decimal::ZERO)
                            }
                            Err(e) => {
                                return Err(BitflyerApiError::InvalidResponse(format!(
                                    "failed to determine stop order fill price: stop parent {parent_order_acceptance_id}: child {} filled \
                                     (executed_size={}) but no execution data and no average_price: {e}",
                                    child.child_order_acceptance_id, child.executed_size
                                )));
                            }
                        };
                        Ok(StopOrderStatus::Executed { price, commission })
                    }
                    None => Ok(if parent.parent_order_state.is_none() {
                        StopOrderStatus::Active
                    } else {
                        StopOrderStatus::Gone
                    }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
