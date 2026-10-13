use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use hmac::{Hmac, Mac};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use urlencoding::encode as url_encode;

use crate::exchange_api::{StopOrderStatus, weighted_avg_fills};

type HmacSha256 = Hmac<Sha256>;

pub type RateLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

#[derive(Debug, Clone, Serialize)]
pub struct SendChildOrderRequest {
    pub product_code: String,
    pub child_order_type: ChildOrderType,
    pub side: Side,
    pub size: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minute_to_expire: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<TimeInForce>,
    #[serde(skip)]
    pub close_position_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChildOrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendChildOrderResponse {
    pub child_order_acceptance_id: String,
}

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

#[derive(Debug, Clone, Deserialize)]
pub struct ChildOrder {
    pub id: u64,
    pub child_order_id: String,
    pub product_code: String,
    pub side: String,
    pub child_order_type: String,
    pub price: Decimal,
    pub average_price: Decimal,
    pub size: Decimal,
    pub child_order_state: ChildOrderState,
    pub expire_date: String,
    pub child_order_date: String,
    pub child_order_acceptance_id: String,
    pub outstanding_size: Decimal,
    pub cancel_size: Decimal,
    pub executed_size: Decimal,
    pub total_commission: Decimal,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChildOrderState {
    Active,
    Completed,
    Canceled,
    Expired,
    Rejected,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Execution {
    pub id: u64,
    pub child_order_id: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub commission: Decimal,
    pub exec_date: String,
    pub child_order_acceptance_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExchangePosition {
    pub product_code: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub commission: Decimal,
    pub swap_point_accumulate: Decimal,
    pub require_collateral: Decimal,
    pub open_date: String,
    pub leverage: Decimal,
    pub pnl: Decimal,
    pub sfd: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Collateral {
    pub collateral: Decimal,
    pub open_position_pnl: Decimal,
    pub require_collateral: Decimal,
    pub keep_rate: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BitflyerErrorBody {
    pub status: i32,
    pub error_message: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Error)]
pub enum BitflyerApiError {
    #[error("invalid order: insufficient funds: {0}")]
    InsufficientFunds(String),
    #[error("invalid api key")]
    InvalidApiKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("failed to send request: rate limited, retry after {retry_after:?}")]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
    #[error("invalid order: not found: {0}")]
    OrderNotFound(String),
    #[error("failed to call bitflyer api: status={status} message={message}")]
    ApiError { status: i32, message: String },
    #[error("failed to send http request: {0}")]
    Http(reqwest::Error),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

impl From<reqwest::Error> for BitflyerApiError {
    fn from(e: reqwest::Error) -> Self {
        BitflyerApiError::Http(e.without_url())
    }
}

impl BitflyerApiError {
    pub fn from_body(body: BitflyerErrorBody) -> Self {
        match body.status {
            -200 | -205 => BitflyerApiError::InsufficientFunds(body.error_message),
            -201 => BitflyerApiError::InvalidApiKey,
            -207 => BitflyerApiError::InvalidSignature,
            -208 => BitflyerApiError::OrderNotFound(body.error_message),
            s => BitflyerApiError::ApiError {
                status: s,
                message: body.error_message,
            },
        }
    }
}

#[derive(Clone)]
pub struct BitflyerPrivateApi {
    base_url: String,
    api_key: String,
    api_secret: String,
    http: reqwest::Client,
    rate_limiter: Option<Arc<RateLimiter>>,
}

impl std::fmt::Debug for BitflyerPrivateApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitflyerPrivateApi")
            .field("base_url", &self.base_url)
            .field("api_key", &"***redacted***")
            .field("api_secret", &"***redacted***")
            .field(
                "rate_limiter",
                if self.rate_limiter.is_some() {
                    &"<enabled>"
                } else {
                    &"<none>"
                },
            )
            .finish_non_exhaustive()
    }
}

impl BitflyerPrivateApi {
    pub fn new(api_key: String, api_secret: String) -> Self {
        const CONSERVATIVE_RATE_LIMIT_REQUESTS_PER_MINUTE: u32 = 30;
        const CONSERVATIVE_RATE_LIMIT_BURST: u32 = 10;
        let limiter = Arc::new(governor::RateLimiter::direct(
            governor::Quota::per_minute(
                NonZeroU32::new(CONSERVATIVE_RATE_LIMIT_REQUESTS_PER_MINUTE).unwrap(),
            )
            .allow_burst(NonZeroU32::new(CONSERVATIVE_RATE_LIMIT_BURST).unwrap()),
        ));
        let mut api =
            Self::with_base_url("https://api.bitflyer.com".to_string(), api_key, api_secret);
        api.rate_limiter = Some(limiter);
        api
    }

    pub(crate) fn with_base_url(base_url: String, api_key: String, api_secret: String) -> Self {
        Self {
            base_url,
            api_key,
            api_secret,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client builder should not fail with basic config"),
            rate_limiter: None,
        }
    }

    #[doc(hidden)]
    pub fn new_for_test(base_url: String, api_key: String, api_secret: String) -> Self {
        Self::with_base_url(base_url, api_key, api_secret)
    }

    #[doc(hidden)]
    pub fn with_rate_limiter(mut self, limiter: Arc<RateLimiter>) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    pub(crate) fn auth_headers(
        &self,
        timestamp: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> HashMap<&'static str, String> {
        let sig = sign(&self.api_secret, timestamp, method, path, body);
        let mut h = HashMap::new();
        h.insert("ACCESS-KEY", self.api_key.clone());
        h.insert("ACCESS-TIMESTAMP", timestamp.to_string());
        h.insert("ACCESS-SIGN", sig);
        h
    }

    fn current_timestamp() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before 1970");
        now.as_secs().to_string()
    }

    pub(crate) async fn request(
        &self,
        method: &str,
        path: &str,
        body_json: &str,
    ) -> Result<String, BitflyerApiError> {
        if let Some(limiter) = &self.rate_limiter {
            limiter.until_ready().await;
        }
        let url = format!("{}{}", self.base_url, path);
        let ts = Self::current_timestamp();
        let headers = self.auth_headers(&ts, method, path, body_json);

        let mut req = match method {
            "GET" => self.http.get(&url),
            "POST" => self.http.post(&url),
            _ => {
                return Err(BitflyerApiError::InvalidResponse(format!(
                    "unsupported method: {method}"
                )));
            }
        };
        for (k, v) in &headers {
            req = req.header(*k, v);
        }
        if method == "POST" {
            req = req
                .header("Content-Type", "application/json")
                .body(body_json.to_string());
        }

        let resp = req.send().await?;
        let status = resp.status();

        if status.as_u16() == 429 {
            let retry_after = parse_retry_after(&resp);
            let _ = resp.text().await;
            return Err(BitflyerApiError::RateLimited { retry_after });
        }

        let text = resp.text().await.map_err(|e| {
            tracing::warn!(method, path, "failed to read response body");
            BitflyerApiError::Http(e.without_url())
        })?;

        if !status.is_success() {
            return match serde_json::from_str::<BitflyerErrorBody>(&text) {
                Ok(body) => Err(BitflyerApiError::from_body(body)),
                Err(_) => {
                    let truncated = truncate_body(&text);
                    tracing::warn!(
                        method,
                        path,
                        status = status.as_u16(),
                        "failed to parse non-2xx response body"
                    );
                    Err(BitflyerApiError::InvalidResponse(format!(
                        "failed to parse error response: non-2xx status {} body (truncated to {MAX_ERROR_BODY_CHARS} chars): {}",
                        status.as_u16(),
                        truncated
                    )))
                }
            };
        }

        if let Ok(body) = serde_json::from_str::<BitflyerErrorBody>(&text)
            && body.status < 0
        {
            return Err(BitflyerApiError::from_body(body));
        }

        Ok(text)
    }

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

    pub async fn get_positions(
        &self,
        product_code: &str,
    ) -> Result<Vec<ExchangePosition>, BitflyerApiError> {
        let path = format!(
            "/v1/me/getpositions?product_code={}",
            url_encode(product_code)
        );
        let text = self.request("GET", &path, "").await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    pub async fn get_collateral(&self) -> Result<Collateral, BitflyerApiError> {
        let text = self.request("GET", "/v1/me/getcollateral", "").await?;
        serde_json::from_str(&text).map_err(|e| {
            BitflyerApiError::InvalidResponse(format!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            ))
        })
    }

    pub async fn fetch_close_sfd(&self, product_code: &str) -> Result<Decimal, BitflyerApiError> {
        let positions = self.get_positions(product_code).await?;
        Ok(positions.iter().map(|p| p.sfd).sum())
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

#[async_trait::async_trait]
impl crate::exchange_api::ExchangeApi for BitflyerPrivateApi {
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

pub(crate) const MAX_ERROR_BODY_CHARS: usize = 512;

pub(crate) fn truncate_body(text: &str) -> String {
    text.chars().take(MAX_ERROR_BODY_CHARS).collect()
}

fn parse_retry_after(resp: &reqwest::Response) -> Option<std::time::Duration> {
    let hdr = resp.headers().get("retry-after")?.to_str().ok()?;
    hdr.parse::<u64>().ok().map(std::time::Duration::from_secs)
}

pub(crate) fn sign(
    api_secret: &str,
    timestamp: &str,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    let mut mac = HmacSha256::new_from_slice(api_secret.as_bytes())
        .expect("HMAC key size is never rejected by Hmac");
    mac.update(timestamp.as_bytes());
    mac.update(method.as_bytes());
    mac.update(path.as_bytes());
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests;
