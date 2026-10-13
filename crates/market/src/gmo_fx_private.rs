use std::num::NonZeroU32;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::{Client, Method};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::bitflyer_private::{
    ChildOrder, ChildOrderState, ChildOrderType, Collateral, ExchangePosition, Execution,
    SendChildOrderRequest, SendChildOrderResponse, Side, truncate_body,
};
use crate::exchange_api::{ExchangeApi, StopOrderStatus, weighted_avg_fills};

type HmacSha256 = Hmac<Sha256>;

pub type RateLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

const DEFAULT_API_URL: &str = "https://forex-api.coin.z.com";

pub(crate) fn sign_path_for(path: &str) -> &str {
    path.split_once('?').map(|(p, _)| p).unwrap_or(path)
}

pub(crate) fn sign(
    api_secret: &str,
    timestamp_ms: i64,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    let msg = format!("{timestamp_ms}{method}{path}{body}");
    let mut mac =
        HmacSha256::new_from_slice(api_secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(msg.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum GmoSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum GmoExecutionType {
    Market,
    Limit,
    Stop,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GmoOrderRequest {
    pub symbol: String,
    pub side: GmoSide,
    pub execution_type: GmoExecutionType,
    pub size: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GmoSettlePosition {
    pub position_id: u64,
    pub size: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GmoCloseRequest {
    pub symbol: String,
    pub side: GmoSide,
    pub execution_type: GmoExecutionType,
    pub settle_position: Vec<GmoSettlePosition>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GmoStopCloseRequest {
    pub symbol: String,
    pub side: GmoSide,
    pub execution_type: GmoExecutionType,
    pub price: String,
    pub settle_position: Vec<GmoSettlePosition>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GmoOrderResponseData {
    pub root_order_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GmoApiResponse<T> {
    pub status: i32,
    #[serde(default)]
    pub messages: Vec<GmoApiMessage>,
    pub data: Option<T>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(crate) struct GmoApiMessage {
    pub message_code: String,
    pub message_string: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GmoOpenPosition {
    pub position_id: u64,
    pub symbol: String,
    pub side: String,
    pub size: Decimal,
    pub price: Decimal,
    pub timestamp: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(crate) struct GmoExecution {
    pub execution_id: u64,
    pub order_id: u64,
    pub position_id: Option<u64>,
    pub symbol: String,
    pub side: String,
    pub settle_type: String,
    pub size: Decimal,
    pub price: Decimal,
    pub loss_gain: Decimal,
    pub fee: Decimal,
    pub timestamp: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(crate) struct GmoAccountAssets {
    pub equity: Decimal,
    pub available_amount: Decimal,
    pub balance: Decimal,
    pub estimated_trade_fee: Decimal,
    pub margin: Decimal,
    pub margin_call_status: String,
    pub margin_ratio: Decimal,
    pub position_loss_gain: Decimal,
    pub total_swap: Decimal,
    pub transferable_amount: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(bound = "T: serde::de::DeserializeOwned")]
pub(crate) struct GmoListResponse<T> {
    #[serde(default = "Vec::new")]
    pub list: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(crate) struct GmoOrder {
    pub order_id: u64,
    pub symbol: String,
    pub side: String,
    pub execution_type: String,
    pub size: Decimal,
    #[serde(default)]
    pub executed_size: Decimal,
    pub price: Decimal,
    pub status: String,
    pub timestamp: String,
}

pub struct GmoFxPrivateApi {
    api_url: String,
    api_key: String,
    api_secret: String,
    http: Client,
    post_limiter: Arc<RateLimiter>,
    get_limiter: Arc<RateLimiter>,
}

const POST_RATE_LIMIT_PER_SECOND: u32 = 1;
const GET_RATE_LIMIT_PER_SECOND: u32 = 6;

impl GmoFxPrivateApi {
    pub fn new(api_key: String, api_secret: String) -> Self {
        let post_limit =
            NonZeroU32::new(POST_RATE_LIMIT_PER_SECOND).expect("rate limit must be non-zero");
        let get_limit =
            NonZeroU32::new(GET_RATE_LIMIT_PER_SECOND).expect("rate limit must be non-zero");
        let post_quota = governor::Quota::per_second(post_limit).allow_burst(post_limit);
        let get_quota = governor::Quota::per_second(get_limit).allow_burst(get_limit);
        Self {
            api_url: DEFAULT_API_URL.to_string(),
            api_key,
            api_secret,
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client build"),
            post_limiter: Arc::new(RateLimiter::direct(post_quota)),
            get_limiter: Arc::new(RateLimiter::direct(get_quota)),
        }
    }

    pub fn with_api_url(mut self, url: String) -> Self {
        self.api_url = url;
        self
    }

    async fn signed_request<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> anyhow::Result<T> {
        let is_post = method == Method::POST;
        if is_post {
            self.post_limiter.until_ready().await;
        } else {
            self.get_limiter.until_ready().await;
        }
        let body_str = body.map(|v| v.to_string()).unwrap_or_default();
        let ts = Utc::now().timestamp_millis();
        let sign_path = sign_path_for(path);
        let sig = sign(&self.api_secret, ts, method.as_str(), sign_path, &body_str);
        let url = format!("{}{}{}", self.api_url, "/private", path);
        let mut req = self
            .http
            .request(method.clone(), &url)
            .header("API-KEY", &self.api_key)
            .header("API-TIMESTAMP", ts.to_string())
            .header("API-SIGN", sig);
        if is_post {
            req = req
                .header("Content-Type", "application/json")
                .body(body_str.clone());
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("failed to send http request to {method} {url}"))?;
        let status = resp.status();
        let text = resp.text().await.context("failed to read response body")?;
        if !status.is_success() {
            anyhow::bail!(
                "failed to call gmo api: status={status} body={}",
                truncate_body(&text)
            );
        }
        let api_resp: GmoApiResponse<T> = serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            )
        })?;
        if api_resp.status != 0 {
            anyhow::bail!(
                "failed to call gmo api: status={} messages={:?}",
                api_resp.status,
                api_resp.messages
            );
        }
        api_resp.data.ok_or_else(|| {
            anyhow::anyhow!("invalid response: data is null: {}", truncate_body(&text))
        })
    }

    async fn post_open_order(
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

    async fn post_close_order(
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

fn gmo_status_to_state(status: &str) -> ChildOrderState {
    match status {
        "EXECUTED" => ChildOrderState::Completed,
        "CANCELED" => ChildOrderState::Canceled,
        "EXPIRED" => ChildOrderState::Expired,
        "REJECTED" => ChildOrderState::Rejected,
        _ => ChildOrderState::Active,
    }
}

fn ensure_market_order(req: &SendChildOrderRequest) -> anyhow::Result<()> {
    if !matches!(req.child_order_type, ChildOrderType::Market) {
        anyhow::bail!("unsupported order type: LIMIT");
    }
    if req.price.is_some() {
        anyhow::bail!("invalid price: must be none for a market order");
    }
    Ok(())
}

fn side_to_gmo(side: Side) -> GmoSide {
    match side {
        Side::Buy => GmoSide::Buy,
        Side::Sell => GmoSide::Sell,
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
