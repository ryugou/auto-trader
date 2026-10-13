use anyhow::Context as _;
use reqwest::Method;

use crate::bitflyer_private::{ChildOrderType, SendChildOrderRequest, SendChildOrderResponse};

use super::http::GmoFxPrivateApi;
use super::types::{
    GmoCloseRequest, GmoExecutionType, GmoOrderRequest, GmoOrderResponseData, GmoSettlePosition,
    side_to_gmo,
};

fn ensure_market_order(req: &SendChildOrderRequest) -> anyhow::Result<()> {
    if !matches!(req.child_order_type, ChildOrderType::Market) {
        anyhow::bail!("unsupported order type: LIMIT");
    }
    if req.price.is_some() {
        anyhow::bail!("invalid price: must be none for a market order");
    }
    Ok(())
}

impl GmoFxPrivateApi {
    pub(super) async fn post_open_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        ensure_market_order(&req)?;
        let body = GmoOrderRequest {
            symbol: req.product_code,
            side: side_to_gmo(req.side),
            execution_type: GmoExecutionType::Market,
            size: req.size.to_string(),
        };
        let body_val = serde_json::to_value(&body)?;
        let data: GmoOrderResponseData = self
            .signed_request(Method::POST, "/v1/order", Some(&body_val))
            .await?;
        Ok(SendChildOrderResponse {
            child_order_acceptance_id: data.root_order_id.to_string(),
        })
    }

    pub(super) async fn post_close_order(
        &self,
        req: SendChildOrderRequest,
        position_id_str: String,
    ) -> anyhow::Result<SendChildOrderResponse> {
        ensure_market_order(&req)?;
        let position_id: u64 = position_id_str
            .parse()
            .context("invalid close_position_id: not a u64")?;
        let body = GmoCloseRequest {
            symbol: req.product_code,
            side: side_to_gmo(req.side),
            execution_type: GmoExecutionType::Market,
            settle_position: vec![GmoSettlePosition {
                position_id,
                size: req.size.to_string(),
            }],
        };
        let body_val = serde_json::to_value(&body)?;
        let data: GmoOrderResponseData = self
            .signed_request(Method::POST, "/v1/closeOrder", Some(&body_val))
            .await?;
        Ok(SendChildOrderResponse {
            child_order_acceptance_id: data.root_order_id.to_string(),
        })
    }
}

#[cfg(test)]
mod tests;
