//! OANDA v3 REST Private API client, implementing `ExchangeApi`.
//!
//! Auth: Bearer token (simple, no HMAC). Account-scoped: every request
//! includes the OANDA account ID in the URL path.
//! Rate limit: 120 req/sec per account.
//!
//! ## Known limitations (tracked)
//!
//! - `send_child_order` sets `positionFill: "DEFAULT"`. The actual effect
//!   depends on the OANDA account's positionFill configuration
//!   (OPEN_ONLY / REDUCE_FIRST / REDUCE_ONLY / DEFAULT). For close/reduce
//!   semantics, configure the OANDA account accordingly or add explicit
//!   positionFill handling here when a differentiating strategy arrives.
//! - `get_child_orders` returns a single-element Vec with incomplete fields
//!   (child_order_type hardcoded to Market, price=0). Only used for
//!   reconciliation today; Trader doesn't call it. Refine when a real
//!   consumer needs richer data.
//! - `get_positions` maps to bitFlyer-shaped `ExchangePosition` where
//!   several fields (commission, require_collateral, pnl, sfd, leverage)
//!   are left zero because OANDA's `/positions/{instrument}` doesn't
//!   expose them uniformly. Pull swap/financing via `/transactions` if
//!   needed.
//! - Seed migration (`20260419000001_oanda_paper_seed.sql`) uses
//!   `ON CONFLICT (id) DO NOTHING`; if an operator inserts another paper
//!   OANDA row with a different UUID, both will coexist. Acceptable for
//!   paper; tighten via a partial unique index if live-OANDA multi-row
//!   risk surfaces.

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};
use rust_decimal::Decimal;
use std::str::FromStr;

use crate::bitflyer_private::{
    ChildOrder, ChildOrderState, ChildOrderType, Collateral, ExchangePosition, Execution,
    SendChildOrderRequest, SendChildOrderResponse,
};
use crate::exchange_api::ExchangeApi;

fn encode_path(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

#[derive(Debug, thiserror::Error)]
pub enum OandaApiError {
    #[error("HTTP request failed: {0}")]
    Http(reqwest::Error),
    #[error("OANDA API error {status}: {body}")]
    Api { status: StatusCode, body: String },
    #[error("response parse error: {0}")]
    Parse(String),
    #[error("invalid config: {0}")]
    Config(String),
}

impl From<reqwest::Error> for OandaApiError {
    fn from(e: reqwest::Error) -> Self {
        // Redact the request URL from the error message to prevent account_id leakage
        // (reqwest::Error.Display includes the full URL, which embeds the account_id)
        OandaApiError::Http(e.without_url())
    }
}

pub struct OandaPrivateApi {
    client: Client,
    base_url: String,   // e.g. "https://api-fxpractice.oanda.com"
    account_id: String, // OANDA account ID (e.g. "101-001-12345-001")
    api_key: String,    // Bearer token
}

impl OandaPrivateApi {
    pub fn new(base_url: String, account_id: String, api_key: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest Client::builder().build()"),
            base_url,
            account_id,
            api_key,
        }
    }

    fn authed(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Accept-Datetime-Format", "RFC3339")
            .header("Content-Type", "application/json")
    }

    /// Produce a log/error-safe snippet of a response body: redacts the
    /// account_id and truncates to MAX_ERR_BODY chars.
    fn sanitize_body_snippet(&self, body: &str) -> String {
        const MAX_ERR_BODY: usize = 512;
        let redacted = if body.contains(&self.account_id) {
            body.replace(&self.account_id, "<REDACTED_ACCOUNT_ID>")
        } else {
            body.to_string()
        };
        let truncated: String = redacted.chars().take(MAX_ERR_BODY).collect();
        if redacted.chars().count() > MAX_ERR_BODY {
            format!(
                "{}... (truncated from {} chars)",
                truncated,
                redacted.chars().count()
            )
        } else {
            truncated
        }
    }

    async fn send_json(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> anyhow::Result<serde_json::Value> {
        let resp = builder.send().await.map_err(OandaApiError::from)?;
        let status = resp.status();
        let text = resp.text().await.map_err(OandaApiError::from)?;
        if !status.is_success() {
            let final_body = self.sanitize_body_snippet(&text);
            return Err(anyhow::Error::from(OandaApiError::Api {
                status,
                body: final_body,
            }));
        }
        serde_json::from_str(&text)
            .map_err(|e| anyhow::Error::from(OandaApiError::Parse(e.to_string())))
    }
}

#[async_trait]
impl ExchangeApi for OandaPrivateApi {
    async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        // Only MARKET orders are supported. Trader never emits LIMIT today;
        // reject explicitly rather than sending a broken LIMIT+FOK combo.
        if !matches!(req.child_order_type, ChildOrderType::Market) {
            anyhow::bail!(
                "OandaPrivateApi only supports MARKET orders currently; LIMIT support is a future addition"
            );
        }

        // Require integer units. OANDA's smallest unit is 1 (= 1 base currency
        // unit). Fractional or zero sizes are never valid and silently rounding
        // them is dangerous.
        use crate::bitflyer_private::Side;
        use rust_decimal::prelude::ToPrimitive;

        if req.size.is_sign_negative() || req.size.is_zero() {
            anyhow::bail!(
                "OandaPrivateApi: size must be positive non-zero: {}",
                req.size
            );
        }
        if req.size.fract() != rust_decimal::Decimal::ZERO {
            anyhow::bail!(
                "OandaPrivateApi: size must be an integer number of units (got {})",
                req.size
            );
        }
        let units_abs: i64 = req.size.to_i64().ok_or_else(|| {
            anyhow::anyhow!("OandaPrivateApi: size {} does not fit in i64", req.size)
        })?;
        let sign = match req.side {
            Side::Buy => 1i64,
            Side::Sell => -1i64,
        };
        let signed_units = sign * units_abs;

        let order_body = serde_json::json!({
            "order": {
                "type": "MARKET",
                "instrument": req.product_code,
                "units": signed_units.to_string(),
                "timeInForce": "FOK",
                "positionFill": "DEFAULT",
            }
        });

        let path = format!("/v3/accounts/{}/orders", encode_path(&self.account_id));
        let body: serde_json::Value = self
            .send_json(self.authed(Method::POST, &path).json(&order_body))
            .await?;

        // For subsequent /orders/{id} calls, prefer orderCreateTransaction.orderID
        // when present and fall back to orderCreateTransaction.id otherwise.
        // `orderID` is not part of the documented OANDA v3 schema today, but we
        // accept it defensively in case OANDA introduces or returns it in some
        // response shapes — falls through to `.id` (the canonical field in
        // current practice) otherwise.
        let tx = body.get("orderCreateTransaction").ok_or_else(|| {
            OandaApiError::Parse("missing orderCreateTransaction in response".to_string())
        })?;
        let order_id = tx
            .get("orderID")
            .and_then(|v| v.as_str())
            .or_else(|| tx.get("id").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                OandaApiError::Parse(
                    "orderCreateTransaction missing both 'orderID' and 'id'".to_string(),
                )
            })?
            .to_string();

        Ok(SendChildOrderResponse {
            child_order_acceptance_id: order_id,
        })
    }

    async fn get_child_orders(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        // OANDA: GET /v3/accounts/{id}/orders/{orderID}
        // Returns { order: {...} }. Map to Vec<ChildOrder> with a single element.
        let path = format!(
            "/v3/accounts/{}/orders/{}",
            encode_path(&self.account_id),
            encode_path(child_order_acceptance_id)
        );
        let body: serde_json::Value = self.send_json(self.authed(Method::GET, &path)).await?;

        let order = body.get("order").ok_or_else(|| {
            OandaApiError::Parse(format!(
                "missing 'order' field in response: {}",
                self.sanitize_body_snippet(&body.to_string())
            ))
        })?;
        Ok(vec![order_json_to_child(order)?])
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        // 1. Fetch order; check state; collect fillingTransactionIDs.
        //    The order object itself doesn't carry per-fill price — that lives
        //    only in the ORDER_FILL transaction record.
        let order_path = format!(
            "/v3/accounts/{}/orders/{}",
            encode_path(&self.account_id),
            encode_path(child_order_acceptance_id)
        );
        let order_body: serde_json::Value = self
            .send_json(self.authed(Method::GET, &order_path))
            .await?;
        let order = order_body.get("order").ok_or_else(|| {
            OandaApiError::Parse(format!(
                "missing 'order' field in response: {}",
                self.sanitize_body_snippet(&order_body.to_string())
            ))
        })?;

        let state = order.get("state").and_then(|v| v.as_str()).unwrap_or("");
        if state == "FILLED" {
            // fall through to fill-fetch logic below
        } else if matches!(state, "CANCELLED" | "REJECTED" | "EXPIRED") {
            // Terminal non-filled state — fail fast so Trader::poll_executions
            // doesn't waste 5 s spinning before timing out.
            let mut details = Vec::new();
            for key in [
                "cancellingTransactionID",
                "cancellationTransactionID",
                "rejectTransactionID",
                "reissueRejectTransactionID",
                "cancelledTime",
                "rejectReason",
            ] {
                if let Some(v) = order.get(key).and_then(|v| v.as_str()) {
                    details.push(format!("{key}={v}"));
                }
            }
            let suffix = if details.is_empty() {
                String::new()
            } else {
                format!(" ({})", details.join(", "))
            };
            anyhow::bail!(
                "order {} reached terminal non-filled state {}{}",
                child_order_acceptance_id,
                state,
                suffix
            );
        } else {
            // Still pending / working — caller will poll again.
            return Ok(Vec::new());
        }
        let fill_ids: Vec<String> = {
            // Prefer array form fillingTransactionIDs; fall back to singular
            // fillingTransactionID; error if neither present for a FILLED order.
            let from_array = order
                .get("fillingTransactionIDs")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            if !from_array.is_empty() {
                from_array
            } else if let Some(id) = order
                .get("fillingTransactionID")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                vec![id.to_string()]
            } else {
                return Err(OandaApiError::Parse(format!(
                    "filled order {} missing fillingTransactionIDs / fillingTransactionID",
                    child_order_acceptance_id
                ))
                .into());
            }
        };

        // 2. For each fill transaction, fetch it and map to an Execution.
        let mut out = Vec::new();
        for tx_id in fill_ids {
            let tx_path = format!(
                "/v3/accounts/{}/transactions/{}",
                encode_path(&self.account_id),
                encode_path(&tx_id)
            );
            let tx_body: serde_json::Value =
                self.send_json(self.authed(Method::GET, &tx_path)).await?;
            let tx = tx_body.get("transaction").ok_or_else(|| {
                OandaApiError::Parse(format!(
                    "missing 'transaction' field in response: {}",
                    self.sanitize_body_snippet(&tx_body.to_string())
                ))
            })?;

            // OANDA fill transaction shape (ORDER_FILL type):
            //   { type: "ORDER_FILL", price, units, time, commission?, ... }
            let price_str = tx.get("price").and_then(|v| v.as_str()).ok_or_else(|| {
                OandaApiError::Parse(format!(
                    "missing price in fill: {}",
                    self.sanitize_body_snippet(&tx.to_string())
                ))
            })?;
            let price = Decimal::from_str(price_str).map_err(|e| {
                OandaApiError::Parse(format!(
                    "failed to parse price '{price_str}': {e} in fill: {}",
                    self.sanitize_body_snippet(&tx.to_string())
                ))
            })?;
            let units_str = tx.get("units").and_then(|v| v.as_str()).ok_or_else(|| {
                OandaApiError::Parse(format!(
                    "missing units in fill: {}",
                    self.sanitize_body_snippet(&tx.to_string())
                ))
            })?;
            let units_signed: i64 = units_str.parse().map_err(|e: std::num::ParseIntError| {
                OandaApiError::Parse(format!(
                    "failed to parse units '{units_str}': {e} in fill: {}",
                    self.sanitize_body_snippet(&tx.to_string())
                ))
            })?;
            let side_str = if units_signed > 0 { "BUY" } else { "SELL" };
            let size = Decimal::from(units_signed.unsigned_abs());
            let commission = tx
                .get("commission")
                .and_then(|v| v.as_str())
                .and_then(|s| Decimal::from_str(s).ok())
                .unwrap_or(Decimal::ZERO);
            let exec_date = tx
                .get("time")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_default();

            out.push(Execution {
                id: 0, // OANDA has no numeric exec id; 0 placeholder
                child_order_id: child_order_acceptance_id.to_string(),
                child_order_acceptance_id: child_order_acceptance_id.to_string(),
                side: side_str.to_string(),
                price,
                size,
                commission,
                exec_date,
            });
        }
        Ok(out)
    }

    async fn get_positions(&self, product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        // OANDA: GET /v3/accounts/{id}/positions/{instrument}
        //        -> { position: { long: {units, averagePrice}, short: {units, averagePrice} } }
        //
        // Long + short can both be non-zero simultaneously (hedged). Emit
        // separate ExchangePosition entries for each non-zero side.
        //
        // ExchangePosition.side is String in bitFlyer types.
        // Fields sfd and swap_point_accumulate have no OANDA equivalent — set to ZERO.
        let path = format!(
            "/v3/accounts/{}/positions/{}",
            encode_path(&self.account_id),
            encode_path(product_code)
        );
        let body: serde_json::Value = self.send_json(self.authed(Method::GET, &path)).await?;
        let pos = body.get("position").ok_or_else(|| {
            OandaApiError::Parse(format!(
                "missing 'position' field in response: {}",
                self.sanitize_body_snippet(&body.to_string())
            ))
        })?;

        let mut out = Vec::new();
        for (side_str, key) in [("BUY", "long"), ("SELL", "short")] {
            let side_obj = match pos.get(key) {
                Some(v) => v,
                None => continue,
            };
            let units_str = side_obj
                .get("units")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            let units: Decimal = Decimal::from_str(units_str)
                .map_err(|_| OandaApiError::Parse(format!("bad units: {units_str}")))?;
            if units == Decimal::ZERO {
                continue;
            }
            let avg_price = side_obj
                .get("averagePrice")
                .and_then(|v| v.as_str())
                .and_then(|s| Decimal::from_str(s).ok())
                .unwrap_or(Decimal::ZERO);
            out.push(ExchangePosition {
                product_code: product_code.to_string(),
                side: side_str.to_string(),
                price: avg_price,
                size: units.abs(),
                commission: Decimal::ZERO,
                swap_point_accumulate: Decimal::ZERO, // no OANDA equivalent
                require_collateral: Decimal::ZERO,
                open_date: String::new(),
                leverage: Decimal::ZERO,
                pnl: Decimal::ZERO,
                sfd: Decimal::ZERO, // no OANDA equivalent
            });
        }
        Ok(out)
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        // OANDA: GET /v3/accounts/{id}/summary
        //   -> { account: { balance, marginAvailable, marginUsed, unrealizedPL, ... } }
        let path = format!("/v3/accounts/{}/summary", encode_path(&self.account_id));
        let body: serde_json::Value = self.send_json(self.authed(Method::GET, &path)).await?;
        let account = body.get("account").ok_or_else(|| {
            OandaApiError::Parse(format!(
                "missing 'account' field in response: {}",
                self.sanitize_body_snippet(&body.to_string())
            ))
        })?;

        let parse_decimal = |key: &str| -> anyhow::Result<Decimal> {
            let value = account.get(key).ok_or_else(|| {
                OandaApiError::Parse(format!(
                    "missing required account field '{key}' in summary response"
                ))
            })?;
            let s = value.as_str().ok_or_else(|| {
                OandaApiError::Parse(format!("account field '{key}' is not a string: {value}"))
            })?;
            Decimal::from_str(s).map_err(|e| {
                anyhow::Error::from(OandaApiError::Parse(format!(
                    "invalid decimal in account field '{key}': '{s}': {e}"
                )))
            })
        };

        Ok(Collateral {
            collateral: parse_decimal("balance")?,
            open_position_pnl: parse_decimal("unrealizedPL")?,
            require_collateral: parse_decimal("marginUsed")?,
            // OANDA uses marginRate differently; keep_rate has no direct equivalent.
            keep_rate: Decimal::ZERO,
        })
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        // OANDA: PUT /v3/accounts/{id}/orders/{orderID}/cancel
        let path = format!(
            "/v3/accounts/{}/orders/{}/cancel",
            encode_path(&self.account_id),
            encode_path(child_order_acceptance_id)
        );
        let _ = self.send_json(self.authed(Method::PUT, &path)).await?;
        Ok(())
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

/// Map OANDA order JSON → `ChildOrder`.
///
/// `ChildOrder.side` and `child_order_state` come from bitflyer_private types.
/// OANDA state is mapped to the closest bitFlyer `ChildOrderState` variant.
/// Fields that OANDA doesn't expose (outstanding_size, cancel_size, sfd, …)
/// receive neutral defaults.
fn order_json_to_child(order: &serde_json::Value) -> anyhow::Result<ChildOrder> {
    let id = order
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OandaApiError::Parse(format!("order missing 'id': {order}")))?
        .to_string();
    let instrument = order
        .get("instrument")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OandaApiError::Parse(format!("order missing 'instrument': {order}")))?
        .to_string();

    // Map OANDA state string to ChildOrderState enum.
    // OANDA states: PENDING, FILLED, TRIGGERED, CANCELLED, EXPIRED, REJECTED.
    let oanda_state = order
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("PENDING");
    let child_order_state = match oanda_state {
        "FILLED" => ChildOrderState::Completed,
        "CANCELLED" => ChildOrderState::Canceled,
        "EXPIRED" => ChildOrderState::Expired,
        "REJECTED" => ChildOrderState::Rejected,
        // TRIGGERED means a stop/take-profit order has fired and is now live
        // as a market/limit order — it is not yet filled, so treat as Active.
        "TRIGGERED" | "PENDING" => ChildOrderState::Active,
        _ => ChildOrderState::Active, // conservative default
    };

    let units_str = order
        .get("units")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OandaApiError::Parse(format!("order missing 'units': {order}")))?;
    let units: i64 = units_str.parse().map_err(|e| {
        OandaApiError::Parse(format!("order 'units' parse error: {e} value={units_str}"))
    })?;
    // ChildOrder.side is String ("BUY"/"SELL") in bitFlyer types.
    let side = if units >= 0 { "BUY" } else { "SELL" }.to_string();
    let size = Decimal::from(units.unsigned_abs());

    Ok(ChildOrder {
        id: 0, // OANDA has no numeric row id; 0 placeholder
        child_order_id: id.clone(),
        child_order_acceptance_id: id,
        product_code: instrument,
        side,
        child_order_type: "MARKET".to_string(),
        price: Decimal::ZERO,
        average_price: order
            .get("averagePrice")
            .and_then(|v| v.as_str())
            .and_then(|s| Decimal::from_str(s).ok())
            .unwrap_or(Decimal::ZERO),
        size,
        child_order_state,
        expire_date: String::new(),
        child_order_date: order
            .get("createTime")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default(),
        outstanding_size: Decimal::ZERO,
        cancel_size: Decimal::ZERO,
        executed_size: match child_order_state {
            ChildOrderState::Completed => size,
            _ => Decimal::ZERO,
        },
        total_commission: Decimal::ZERO,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn positions_path_escapes_special_chars() {
        let server = MockServer::start().await;
        // Register mock for the PRE-encoded path so we can detect proper encoding
        Mock::given(method("GET"))
            .and(path("/v3/accounts/101-001-12345-001/positions/USD%2FJPY"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "position": {
                    "long":  { "units": "0", "averagePrice": "0" },
                    "short": { "units": "0", "averagePrice": "0" }
                }
            })))
            .mount(&server)
            .await;

        let api = OandaPrivateApi::new(
            server.uri(),
            "101-001-12345-001".to_string(),
            "fake-token".to_string(),
        );
        // Input with a slash — would normally break the URL path if not encoded
        let ps = api.get_positions("USD/JPY").await.unwrap();
        assert_eq!(ps.len(), 0); // both zero units → empty
    }

    #[tokio::test]
    async fn send_json_redacts_account_id_in_error_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v3/accounts/101-001-12345-001/orders"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "errorMessage": "Invalid request for accountID 101-001-12345-001",
                "accountID": "101-001-12345-001"
            })))
            .mount(&server)
            .await;

        let api = OandaPrivateApi::new(
            server.uri(),
            "101-001-12345-001".to_string(),
            "fake-token".to_string(),
        );
        let err = api
            .send_child_order(SendChildOrderRequest {
                product_code: "USD_JPY".to_string(),
                child_order_type: ChildOrderType::Market,
                side: crate::bitflyer_private::Side::Buy,
                size: rust_decimal::Decimal::from(1000),
                price: None,
                minute_to_expire: None,
                time_in_force: None,
                close_position_id: None,
            })
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("101-001-12345-001"),
            "account_id leaked: {msg}"
        );
        assert!(msg.contains("REDACTED"), "redaction marker missing: {msg}");
    }

    #[tokio::test]
    async fn send_json_redacts_account_id_even_past_truncation_boundary() {
        let server = MockServer::start().await;
        // Body where account_id appears around char 600 (past 512 boundary).
        let padding = "x".repeat(550);
        let body = format!(
            "{{\"errorMessage\": \"{}\", \"accountID\": \"101-001-12345-001\"}}",
            padding
        );
        Mock::given(method("POST"))
            .and(path("/v3/accounts/101-001-12345-001/orders"))
            .respond_with(ResponseTemplate::new(400).set_body_string(body))
            .mount(&server)
            .await;

        let api = OandaPrivateApi::new(
            server.uri(),
            "101-001-12345-001".to_string(),
            "fake-token".to_string(),
        );
        let err = api
            .send_child_order(SendChildOrderRequest {
                product_code: "USD_JPY".to_string(),
                child_order_type: ChildOrderType::Market,
                side: crate::bitflyer_private::Side::Buy,
                size: rust_decimal::Decimal::from(1000),
                price: None,
                minute_to_expire: None,
                time_in_force: None,
                close_position_id: None,
            })
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("101-001-12345-001"),
            "account_id leaked past truncation: {msg}"
        );
    }

    #[tokio::test]
    async fn parse_error_redacts_account_id_in_body() {
        let server = MockServer::start().await;
        // Return a 200 but with a malformed body containing the account_id
        Mock::given(method("GET"))
            .and(path("/v3/accounts/101-001-12345-001/positions/USD%2FJPY"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "wrong_field": "whatever 101-001-12345-001"
                // missing "position" field → parse error
            })))
            .mount(&server)
            .await;

        let api = OandaPrivateApi::new(
            server.uri(),
            "101-001-12345-001".to_string(),
            "fake-token".to_string(),
        );
        let err = api.get_positions("USD/JPY").await.unwrap_err();
        assert!(
            !err.to_string().contains("101-001-12345-001"),
            "leaked: {err}"
        );
    }
}
