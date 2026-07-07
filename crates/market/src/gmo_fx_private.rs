//! GMO Coin Forex (FX) Private API client.
//!
//! Spec: <https://api.coin.z.com/fxdocs/en/>
//!
//! - Base URL: `https://forex-api.coin.z.com` (paths are prefixed with `/private`).
//! - Auth: `API-SIGN = HMAC-SHA256(secret, timestamp_ms + method + path + body)`.
//!   `path` is the API path *without* the `/private` prefix (e.g. `/v1/order`).
//! - Rate limits: 1 POST/sec, 6 GET/sec per access key. Enforced here via
//!   `governor` token-bucket limiters; the trait dispatches to the right
//!   limiter from the HTTP method.

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
    SendChildOrderRequest, SendChildOrderResponse, Side,
};
use crate::exchange_api::{ExchangeApi, StopOrderStatus, weighted_avg_fills};

type HmacSha256 = Hmac<Sha256>;

pub type RateLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

const DEFAULT_API_URL: &str = "https://forex-api.coin.z.com";

/// Strip `?query` from a path for GMO FX HMAC signing (GMO's auth scheme
/// signs the path component only, not the query string). Pure function so
/// callers and tests can verify the contract without going through HTTP.
pub(crate) fn sign_path_for(path: &str) -> &str {
    path.split_once('?').map(|(p, _)| p).unwrap_or(path)
}

/// HMAC-SHA256(secret, timestamp_ms || method || path || body), hex-encoded.
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

// ---------------------------------------------------------------------------
// GMO FX request / response types
// ---------------------------------------------------------------------------

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

/// `POST /private/v1/closeOrder` (STOP) リクエスト本体。
///
/// TODO(live-verify): STOP closeOrder の正確な body 形 (top-level `size` の
/// 要否、`price` の桁規約) を公式 doc / 実 API で確認する (Task 4.0 で
/// closeOrder の body 詳細は JS レンダリングにより未確認)。MARKET closeOrder と
/// 同形 + `price` を採用。price の tick 丸めは PositionSizer 側で完了済み。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GmoStopCloseRequest {
    pub symbol: String,
    pub side: GmoSide,
    pub execution_type: GmoExecutionType, // STOP
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
#[allow(dead_code)] // fields appear in the Debug-derived error message
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

/// `GET /v1/executions` element. `position_id` / `settle_type` / `loss_gain` /
/// `symbol` are part of the wire format but not consumed by the trader yet;
/// keep them deserialized so the Debug-derived shape stays useful for logs.
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

/// `GET /v1/account/assets` response. Only `balance` / `margin` /
/// `position_loss_gain` / `margin_ratio` map into the shared `Collateral`
/// type; the remaining fields stay for future use + debug visibility.
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

/// `GET /v1/orders?orderId={id}` list element. Shape per
/// <https://api.coin.z.com/fxdocs/en/#order-status>.
/// `executed_size` defaults to 0 because GMO omits the field when no
/// execution has occurred yet.
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

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

pub struct GmoFxPrivateApi {
    api_url: String,
    api_key: String,
    api_secret: String,
    http: Client,
    post_limiter: Arc<RateLimiter>,
    get_limiter: Arc<RateLimiter>,
}

impl GmoFxPrivateApi {
    pub fn new(api_key: String, api_secret: String) -> Self {
        let one = NonZeroU32::new(1).expect("1 > 0");
        let six = NonZeroU32::new(6).expect("6 > 0");
        let post_quota = governor::Quota::per_second(one).allow_burst(one);
        let get_quota = governor::Quota::per_second(six).allow_burst(six);
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
        // GMO Coin FX signs the path **without** the query string. Reference:
        // https://api.coin.z.com/fxdocs/en/#authentication-required and the
        // official sample code that uses `path` (not including `?` onwards).
        // The query string still appears in the URL we send; only the signing
        // input is stripped.
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
            .with_context(|| format!("{method} {url}"))?;
        let status = resp.status();
        let text = resp.text().await.context("read response body")?;
        if !status.is_success() {
            anyhow::bail!("GMO FX HTTP {status}: {}", truncate(&text, 500));
        }
        let api_resp: GmoApiResponse<T> = serde_json::from_str(&text)
            .with_context(|| format!("parse GMO response: {}", truncate(&text, 500)))?;
        if api_resp.status != 0 {
            anyhow::bail!(
                "GMO FX API error status={} messages={:?}",
                api_resp.status,
                api_resp.messages
            );
        }
        api_resp.data.ok_or_else(|| {
            anyhow::anyhow!(
                "GMO FX API success but data is null: {}",
                truncate(&text, 500)
            )
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
            .with_context(|| format!("close_position_id is not a u64: {position_id_str}"))?;
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

/// Map GMO order status string → bitFlyer-shaped `ChildOrderState`.
///
/// GMO terminal: `EXECUTED` / `CANCELED` / `EXPIRED` / `REJECTED`.
/// GMO active:   `WAITING` / `ORDERED` / `MODIFYING` / `CANCELLING`.
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
        anyhow::bail!(
            "GmoFxPrivateApi only supports MARKET orders currently; LIMIT support is a future addition"
        );
    }
    if req.price.is_some() {
        anyhow::bail!(
            "GmoFxPrivateApi: price must be None for MARKET orders (got {:?})",
            req.price
        );
    }
    Ok(())
}

fn side_to_gmo(side: Side) -> GmoSide {
    match side {
        Side::Buy => GmoSide::Buy,
        Side::Sell => GmoSide::Sell,
    }
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
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
        // trader::cleanup_unfilled_order relies on `child_order_state` to
        // decide Completed vs Canceled/Expired/Rejected vs Active. Map GMO
        // `/v1/orders?orderId={id}.data.list[].status` → `ChildOrderState`
        // and synthesise a minimal ChildOrder; an empty list would otherwise
        // be treated as "no order record found → cleanup/cancel".
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
        let order_id: u64 = child_order_acceptance_id.parse().with_context(|| {
            format!("cancel: acceptance_id is not a u64: {child_order_acceptance_id}")
        })?;
        // GMO FX `/v1/cancelOrder` body per official docs: `{ "orderId": <int> }`.
        // The product_code is intentionally ignored — adding a `symbol` field
        // risks strict-body validators rejecting the request.
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
            // Match the just-opened position by symbol+side+size+timestamp.
            // Symbol filter is enforced by the GMO query string; double-check
            // side and size here so a concurrent same-symbol open from another
            // strategy / manual trade cannot bind the wrong positionId.
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

    /// `POST /private/v1/closeOrder` (executionType=STOP) で SL ストップを置く。
    /// GMO は netting なので `position_id` (settlePosition) が必須。None なら bail。
    async fn place_stop_order(
        &self,
        product_code: &str,
        close_side: Side,
        size: Decimal,
        trigger_price: Decimal,
        position_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let position_id_str = position_id.ok_or_else(|| {
            anyhow::anyhow!(
                "GMO FX place_stop_order requires a position_id (netting close); got None"
            )
        })?;
        let position_id: u64 = position_id_str.parse().with_context(|| {
            format!("place_stop_order: position_id is not a u64: {position_id_str}")
        })?;
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

    /// `POST /private/v1/cancelOrder` `{ "orderId": <int> }`。
    /// 「既に約定済み」を示すエラーはそのまま Err で返す (呼び出し側が
    /// status を再確認する契約 — Task 4.5 の close ガード)。
    async fn cancel_stop_order(
        &self,
        _product_code: &str,
        stop_order_id: &str,
    ) -> anyhow::Result<()> {
        let order_id: u64 = stop_order_id
            .parse()
            .with_context(|| format!("cancel_stop_order: id is not a u64: {stop_order_id}"))?;
        let body = serde_json::json!({ "orderId": order_id });
        let _: serde_json::Value = self
            .signed_request(Method::POST, "/v1/cancelOrder", Some(&body))
            .await?;
        Ok(())
    }

    /// `GET /private/v1/orders?orderId=...` の status で状態判定。
    /// EXECUTED → `/v1/executions` を加重平均して `Executed`。
    /// WAITING/ORDERED/MODIFYING/CANCELLING → `Active`。
    /// CANCELED/EXPIRED/REJECTED または一覧空 → `Gone`。
    ///
    /// TODO(live-verify): `/v1/executions` のレスポンス (price/size/fee) は
    /// Task 4.0 で未確認 (JS レンダリング)。GMO 既存 `get_executions` の
    /// フィールド規約 (`GmoExecution`) に合わせているが実 API で最終確認する。
    async fn stop_order_status(
        &self,
        _product_code: &str,
        stop_order_id: &str,
    ) -> anyhow::Result<StopOrderStatus> {
        let path = format!("/v1/orders?orderId={stop_order_id}");
        let data: GmoListResponse<GmoOrder> = self.signed_request(Method::GET, &path, None).await?;
        let Some(order) = data.list.into_iter().next() else {
            // GMO は失効/取消済み注文を /orders から落とす。
            return Ok(StopOrderStatus::Gone);
        };
        match order.status.as_str() {
            "EXECUTED" => {
                let epath = format!("/v1/executions?orderId={stop_order_id}");
                let execs: GmoListResponse<GmoExecution> =
                    self.signed_request(Method::GET, &epath, None).await?;
                let total_fee: Decimal = execs.list.iter().map(|e| e.fee).sum();
                let price =
                    match weighted_avg_fills(execs.list.iter().map(|e| (e.price, e.size, e.fee))) {
                        Ok((avg_price, _total_size, _total_fee)) => avg_price,
                        // 約定明細が空 (もしくは size 合計 0) でも EXECUTED なら
                        // order.price を fallback。
                        Err(_) => order.price,
                    };
                Ok(StopOrderStatus::Executed {
                    price,
                    commission: total_fee,
                })
            }
            "CANCELED" | "EXPIRED" | "REJECTED" => Ok(StopOrderStatus::Gone),
            // WAITING / ORDERED / MODIFYING / CANCELLING → まだ有効。
            _ => Ok(StopOrderStatus::Active),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitflyer_private::ChildOrderType;
    use rust_decimal_macros::dec;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn sign_matches_known_hmac_sha256_vector() {
        // python3 -c "import hmac, hashlib; print(hmac.new(
        //   b'test_secret', b'1700000000GET/v1/account/assets',
        //   hashlib.sha256).hexdigest())"
        let sig = sign("test_secret", 1700000000, "GET", "/v1/account/assets", "");
        assert_eq!(
            sig,
            "1c3cbd89febce462e71e5f7d265ab674f617e6d4449ba5e665b46d1234bedbca"
        );
    }

    #[test]
    fn sign_includes_post_body() {
        let with_body = sign("s", 1, "POST", "/p", "{}");
        let without_body = sign("s", 1, "POST", "/p", "");
        assert_ne!(with_body, without_body);
    }

    #[test]
    fn sign_path_for_strips_query_string() {
        // GMO Coin FX HMAC signs the path WITHOUT the query string.
        // Confirms `sign_path_for` returns identical input for paths with and
        // without `?symbol=...`, so the same HMAC is produced regardless of
        // query content.
        assert_eq!(
            sign_path_for("/v1/openPositions?symbol=USD_JPY"),
            "/v1/openPositions"
        );
        assert_eq!(sign_path_for("/v1/account/assets"), "/v1/account/assets");
        // Multiple `?` (technically malformed but defensive) — strip at first.
        assert_eq!(sign_path_for("/v1/x?a=1?b=2"), "/v1/x");
    }

    #[test]
    fn sign_produces_same_hash_regardless_of_query_string() {
        // The signing input is identical whether or not the path carries a
        // query string. Lock this behavior in so a future refactor that
        // accidentally signs the full URL is caught by tests.
        let with_query = sign(
            "s",
            1700000000,
            "GET",
            sign_path_for("/v1/openPositions?symbol=USD_JPY"),
            "",
        );
        let without_query = sign("s", 1700000000, "GET", "/v1/openPositions", "");
        assert_eq!(with_query, without_query);
    }

    #[test]
    fn open_order_request_serializes_to_camelcase() {
        let req = GmoOrderRequest {
            symbol: "USD_JPY".into(),
            side: GmoSide::Buy,
            execution_type: GmoExecutionType::Market,
            size: "1000".into(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["symbol"], "USD_JPY");
        assert_eq!(json["side"], "BUY");
        assert_eq!(json["executionType"], "MARKET");
        assert_eq!(json["size"], "1000");
    }

    #[test]
    fn close_order_request_serializes_settle_position() {
        let req = GmoCloseRequest {
            symbol: "USD_JPY".into(),
            side: GmoSide::Sell,
            execution_type: GmoExecutionType::Market,
            settle_position: vec![GmoSettlePosition {
                position_id: 12345,
                size: "1000".into(),
            }],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["settlePosition"][0]["positionId"], 12345);
        assert_eq!(json["settlePosition"][0]["size"], "1000");
    }

    #[tokio::test]
    async fn signed_request_attaches_api_key_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/private/v1/account/assets"))
            .and(header("API-KEY", "k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0,
                "data": {
                    "equity": "100000", "availableAmount": "90000",
                    "balance": "100000", "estimatedTradeFee": "0",
                    "margin": "10000", "marginCallStatus": "NORMAL",
                    "marginRatio": "10.0", "positionLossGain": "0",
                    "totalSwap": "0", "transferableAmount": "90000"
                }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let collateral = api.get_collateral().await.unwrap();
        assert_eq!(collateral.collateral, dec!(100000));
        assert_eq!(collateral.keep_rate, dec!(10.0));
    }

    #[tokio::test]
    async fn send_open_order_posts_v1_order_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/private/v1/order"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0, "data": { "rootOrderId": 9876 }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let resp = api
            .send_child_order(SendChildOrderRequest {
                product_code: "USD_JPY".into(),
                child_order_type: ChildOrderType::Market,
                side: Side::Buy,
                size: dec!(1000),
                price: None,
                minute_to_expire: None,
                time_in_force: None,
                close_position_id: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.child_order_acceptance_id, "9876");
    }

    #[tokio::test]
    async fn send_close_order_posts_v1_close_order_with_position_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/private/v1/closeOrder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0, "data": { "rootOrderId": 5555 }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let resp = api
            .send_child_order(SendChildOrderRequest {
                product_code: "USD_JPY".into(),
                child_order_type: ChildOrderType::Market,
                side: Side::Sell,
                size: dec!(1000),
                price: None,
                minute_to_expire: None,
                time_in_force: None,
                close_position_id: Some("123".into()),
            })
            .await
            .unwrap();
        assert_eq!(resp.child_order_acceptance_id, "5555");
    }

    #[tokio::test]
    async fn resolve_position_id_returns_newest_position_after_cutoff() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/private/v1/openPositions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0,
                "data": {
                    "list": [
                        { "positionId": 100, "symbol": "USD_JPY", "side": "BUY",
                          "size": "1000", "price": "150.0",
                          "timestamp": "2026-05-15T10:00:00Z" },
                        { "positionId": 101, "symbol": "USD_JPY", "side": "BUY",
                          "size": "1000", "price": "150.1",
                          "timestamp": "2026-05-15T11:00:00Z" }
                    ]
                }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let after = "2026-05-15T09:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let pid = api
            .resolve_position_id("USD_JPY", after, Side::Buy, dec!(1000))
            .await
            .unwrap();
        assert_eq!(pid.as_deref(), Some("101"));
    }

    #[tokio::test]
    async fn resolve_position_id_excludes_positions_before_cutoff() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/private/v1/openPositions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0,
                "data": {
                    "list": [
                        { "positionId": 100, "symbol": "USD_JPY", "side": "BUY",
                          "size": "1000", "price": "150.0",
                          "timestamp": "2026-05-15T08:00:00Z" }
                    ]
                }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let after = "2026-05-15T09:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let pid = api
            .resolve_position_id("USD_JPY", after, Side::Buy, dec!(1000))
            .await
            .unwrap();
        assert_eq!(pid, None);
    }

    #[tokio::test]
    async fn resolve_position_id_skips_unrelated_side_and_size() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/private/v1/openPositions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0,
                "data": {
                    "list": [
                        // Newer but wrong side — must be skipped.
                        { "positionId": 200, "symbol": "USD_JPY", "side": "SELL",
                          "size": "1000", "price": "150.0",
                          "timestamp": "2026-05-15T12:00:00Z" },
                        // Newer but wrong size — must be skipped.
                        { "positionId": 201, "symbol": "USD_JPY", "side": "BUY",
                          "size": "500", "price": "150.0",
                          "timestamp": "2026-05-15T11:30:00Z" },
                        // Exact match — should be returned.
                        { "positionId": 202, "symbol": "USD_JPY", "side": "BUY",
                          "size": "1000", "price": "150.0",
                          "timestamp": "2026-05-15T11:00:00Z" }
                    ]
                }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let after = "2026-05-15T09:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let pid = api
            .resolve_position_id("USD_JPY", after, Side::Buy, dec!(1000))
            .await
            .unwrap();
        assert_eq!(pid.as_deref(), Some("202"));
    }

    #[tokio::test]
    async fn api_error_status_propagates_as_anyhow_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/private/v1/order"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 1,
                "messages": [{ "messageCode": "ERR-100", "messageString": "invalid" }],
                "data": null
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let err = api
            .send_child_order(SendChildOrderRequest {
                product_code: "USD_JPY".into(),
                child_order_type: ChildOrderType::Market,
                side: Side::Buy,
                size: dec!(1000),
                price: None,
                minute_to_expire: None,
                time_in_force: None,
                close_position_id: None,
            })
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("status=1"), "msg: {msg}");
    }

    #[tokio::test]
    async fn requires_close_position_id_is_true() {
        let api = GmoFxPrivateApi::new("k".into(), "s".into());
        assert!(api.requires_close_position_id());
    }

    #[tokio::test]
    async fn limit_open_order_is_rejected() {
        let api = GmoFxPrivateApi::new("k".into(), "s".into());
        let err = api
            .send_child_order(SendChildOrderRequest {
                product_code: "USD_JPY".into(),
                child_order_type: ChildOrderType::Limit,
                side: Side::Buy,
                size: dec!(1000),
                price: Some(dec!(150.0)),
                minute_to_expire: None,
                time_in_force: None,
                close_position_id: None,
            })
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("MARKET"),
            "should mention MARKET-only: {err}"
        );
    }

    #[tokio::test]
    async fn get_child_orders_maps_gmo_status_to_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/private/v1/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0,
                "data": {
                    "list": [
                        {
                            "orderId": 9876,
                            "symbol": "USD_JPY",
                            "side": "BUY",
                            "executionType": "MARKET",
                            "size": "1000",
                            "executedSize": "1000",
                            "price": "150.0",
                            "status": "EXECUTED",
                            "timestamp": "2026-05-15T10:00:00Z"
                        }
                    ]
                }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let orders = api.get_child_orders("USD_JPY", "9876").await.unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(
            orders[0].child_order_state,
            crate::bitflyer_private::ChildOrderState::Completed
        );
        assert_eq!(orders[0].id, 9876);
        assert_eq!(orders[0].executed_size, dec!(1000));
    }

    #[test]
    fn gmo_status_to_state_maps_each_terminal_status() {
        assert_eq!(gmo_status_to_state("EXECUTED"), ChildOrderState::Completed);
        assert_eq!(gmo_status_to_state("CANCELED"), ChildOrderState::Canceled);
        assert_eq!(gmo_status_to_state("EXPIRED"), ChildOrderState::Expired);
        assert_eq!(gmo_status_to_state("REJECTED"), ChildOrderState::Rejected);
        assert_eq!(gmo_status_to_state("ORDERED"), ChildOrderState::Active);
        assert_eq!(gmo_status_to_state("WAITING"), ChildOrderState::Active);
    }

    // --- Task 4.3: STOP close-order (place/cancel/status) ---

    #[test]
    fn stop_close_request_serializes_stop_price_and_settle_position() {
        let req = GmoStopCloseRequest {
            symbol: "USD_JPY".into(),
            side: GmoSide::Sell,
            execution_type: GmoExecutionType::Stop,
            price: "156.500".into(),
            settle_position: vec![GmoSettlePosition {
                position_id: 123456,
                size: "1592".into(),
            }],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["symbol"], "USD_JPY");
        assert_eq!(json["side"], "SELL");
        assert_eq!(json["executionType"], "STOP");
        assert_eq!(json["price"], "156.500");
        assert_eq!(json["settlePosition"][0]["positionId"], 123456);
        assert_eq!(json["settlePosition"][0]["size"], "1592");
    }

    #[tokio::test]
    async fn place_stop_order_posts_close_order_stop_and_returns_order_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/private/v1/closeOrder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0, "data": { "rootOrderId": 778899 }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let id = api
            .place_stop_order(
                "USD_JPY",
                Side::Sell,
                dec!(1592),
                dec!(156.500),
                Some("123456"),
            )
            .await
            .unwrap();
        assert_eq!(id, "778899");
    }

    #[tokio::test]
    async fn place_stop_order_without_position_id_bails() {
        let api = GmoFxPrivateApi::new("k".into(), "s".into());
        let err = api
            .place_stop_order("USD_JPY", Side::Sell, dec!(1592), dec!(156.5), None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("position_id"),
            "should mention position_id: {err}"
        );
    }

    #[tokio::test]
    async fn cancel_stop_order_posts_cancel_order() {
        let server = MockServer::start().await;
        // GMO cancelOrder は成功時 data に orderId を返す (cancel_child_order と
        // 同じ signed_request 契約: data が null だと bail する)。
        Mock::given(method("POST"))
            .and(path("/private/v1/cancelOrder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0, "data": 778899
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        api.cancel_stop_order("USD_JPY", "778899").await.unwrap();
    }

    #[tokio::test]
    async fn stop_order_status_active_for_ordered() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/private/v1/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0,
                "data": { "list": [{
                    "orderId": 778899, "symbol": "USD_JPY", "side": "SELL",
                    "executionType": "STOP", "size": "1592", "executedSize": "0",
                    "price": "156.500", "status": "ORDERED",
                    "timestamp": "2026-07-07T00:00:00Z"
                }] }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let status = api.stop_order_status("USD_JPY", "778899").await.unwrap();
        assert_eq!(status, StopOrderStatus::Active);
    }

    #[tokio::test]
    async fn stop_order_status_gone_for_empty_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/private/v1/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0, "data": { "list": [] }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let status = api.stop_order_status("USD_JPY", "778899").await.unwrap();
        assert_eq!(status, StopOrderStatus::Gone);
    }

    #[tokio::test]
    async fn stop_order_status_executed_aggregates_executions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/private/v1/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0,
                "data": { "list": [{
                    "orderId": 778899, "symbol": "USD_JPY", "side": "SELL",
                    "executionType": "STOP", "size": "1592", "executedSize": "1592",
                    "price": "156.500", "status": "EXECUTED",
                    "timestamp": "2026-07-07T00:00:00Z"
                }] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/private/v1/executions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": 0,
                "data": { "list": [
                    { "executionId": 1, "orderId": 778899, "positionId": 123456,
                      "symbol": "USD_JPY", "side": "SELL", "settleType": "CLOSE",
                      "size": "796", "price": "156.510", "lossGain": "0", "fee": "3",
                      "timestamp": "2026-07-07T00:00:00Z" },
                    { "executionId": 2, "orderId": 778899, "positionId": 123456,
                      "symbol": "USD_JPY", "side": "SELL", "settleType": "CLOSE",
                      "size": "796", "price": "156.490", "lossGain": "0", "fee": "3",
                      "timestamp": "2026-07-07T00:00:01Z" }
                ] }
            })))
            .mount(&server)
            .await;
        let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
        let status = api.stop_order_status("USD_JPY", "778899").await.unwrap();
        match status {
            StopOrderStatus::Executed { price, commission } => {
                // (156.510*796 + 156.490*796) / 1592 = 156.500
                assert_eq!(price, dec!(156.500));
                assert_eq!(commission, dec!(6));
            }
            other => panic!("expected Executed, got {other:?}"),
        }
    }
}
