//! OANDA v3 REST Private API client, implementing `ExchangeApi`.
//!
//! Auth: Bearer token (simple, no HMAC). Account-scoped: every request
//! includes the OANDA account ID in the URL path.
//! Rate limit: 120 req/sec per account.

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};
use rust_decimal::Decimal;
use std::str::FromStr;

use crate::bitflyer_private::{
    ChildOrder, ChildOrderState, ChildOrderType, Collateral, ExchangePosition, Execution,
    SendChildOrderRequest, SendChildOrderResponse,
};
use crate::exchange_api::ExchangeApi;

#[derive(Debug, thiserror::Error)]
pub enum OandaApiError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("OANDA API error {status}: {body}")]
    Api { status: StatusCode, body: String },
    #[error("response parse error: {0}")]
    Parse(String),
    #[error("invalid config: {0}")]
    Config(String),
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
                .expect("reqwest Client::new"),
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

    async fn send_json(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> anyhow::Result<serde_json::Value> {
        let resp = builder.send().await.map_err(OandaApiError::Http)?;
        let status = resp.status();
        let text = resp.text().await.map_err(OandaApiError::Http)?;
        if !status.is_success() {
            return Err(anyhow::Error::from(OandaApiError::Api {
                status,
                body: text,
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
        // Map bitFlyer-shaped request to OANDA order:
        //   Side::Buy + size  => signed units = +size (long)
        //   Side::Sell + size => signed units = -size (short)
        // OANDA units is integer (1 unit = 1 base currency unit).
        // `size` on SendChildOrderRequest is Decimal; truncate to i64.
        use crate::bitflyer_private::Side;
        let sign = match req.side {
            Side::Buy => 1i64,
            Side::Sell => -1i64,
        };
        let units_abs: i64 = req
            .size
            .to_string()
            .parse::<f64>()
            .map_err(|_| OandaApiError::Config(format!("bad size: {}", req.size)))?
            as i64;
        let signed_units = sign * units_abs;

        let order_type = match req.child_order_type {
            ChildOrderType::Market => "MARKET",
            ChildOrderType::Limit => "LIMIT",
        };

        let mut order_body = serde_json::json!({
            "order": {
                "type": order_type,
                "instrument": req.product_code,
                "units": signed_units.to_string(),
                "timeInForce": "FOK",
                "positionFill": "DEFAULT",
            }
        });
        if matches!(req.child_order_type, ChildOrderType::Limit)
            && let Some(price) = req.price
        {
            order_body["order"]["price"] = serde_json::json!(price.to_string());
        }

        let path = format!("/v3/accounts/{}/orders", self.account_id);
        let body: serde_json::Value = self
            .send_json(self.authed(Method::POST, &path).json(&order_body))
            .await?;

        // orderCreateTransaction.id is the canonical order identifier.
        let order_id = body
            .pointer("/orderCreateTransaction/id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                OandaApiError::Parse(format!("missing orderCreateTransaction.id: {body}"))
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
            self.account_id, child_order_acceptance_id
        );
        let body: serde_json::Value = self.send_json(self.authed(Method::GET, &path)).await?;

        let order = body
            .get("order")
            .ok_or_else(|| OandaApiError::Parse(format!("missing 'order' field: {body}")))?;
        Ok(vec![order_json_to_child(order)?])
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        // OANDA: fills come back as orderFillTransaction inside the
        // transaction list. For a filled MARKET order, the fill id is
        // orderID + "FILL" suffix, but safer: query the order and
        // look for state=FILLED + averagePrice + filledTime.
        //
        // For v1 (wiremock-driven), GET the order and if its state is
        // FILLED, synthesize a single Execution.
        let path = format!(
            "/v3/accounts/{}/orders/{}",
            self.account_id, child_order_acceptance_id
        );
        let body: serde_json::Value = self.send_json(self.authed(Method::GET, &path)).await?;
        let order = body
            .get("order")
            .ok_or_else(|| OandaApiError::Parse(format!("missing 'order' field: {body}")))?;
        let state = order
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("PENDING");
        if state != "FILLED" {
            return Ok(Vec::new());
        }

        // Extract filling info. OANDA embeds filledTime + averagePrice in the order.
        let price = order
            .get("averagePrice")
            .and_then(|v| v.as_str())
            .and_then(|s| Decimal::from_str(s).ok())
            .ok_or_else(|| OandaApiError::Parse(format!("missing averagePrice: {order}")))?;
        let units: i64 = order
            .get("units")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| OandaApiError::Parse(format!("missing units: {order}")))?;
        // ChildOrder.side / Execution.side are String in bitFlyer types.
        let side = if units > 0 { "BUY" } else { "SELL" }.to_string();
        let size = Decimal::from(units.unsigned_abs());

        // Commission/financing — OANDA reports these in the underlying
        // transaction record. Leave as zero; callers can fold in later
        // via get_positions → financing if needed.
        Ok(vec![Execution {
            id: 0, // OANDA has no numeric exec id; 0 placeholder
            child_order_id: child_order_acceptance_id.to_string(),
            child_order_acceptance_id: child_order_acceptance_id.to_string(),
            side,
            price,
            size,
            commission: Decimal::ZERO,
            exec_date: order
                .get("filledTime")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_default(),
        }])
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
            self.account_id, product_code
        );
        let body: serde_json::Value = self.send_json(self.authed(Method::GET, &path)).await?;
        let pos = body
            .get("position")
            .ok_or_else(|| OandaApiError::Parse(format!("missing 'position' field: {body}")))?;

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
        let path = format!("/v3/accounts/{}/summary", self.account_id);
        let body: serde_json::Value = self.send_json(self.authed(Method::GET, &path)).await?;
        let account = body
            .get("account")
            .ok_or_else(|| OandaApiError::Parse(format!("missing 'account' field: {body}")))?;

        let parse_decimal = |key: &str| -> Decimal {
            account
                .get(key)
                .and_then(|v| v.as_str())
                .and_then(|s| Decimal::from_str(s).ok())
                .unwrap_or(Decimal::ZERO)
        };

        Ok(Collateral {
            collateral: parse_decimal("balance"),
            open_position_pnl: parse_decimal("unrealizedPL"),
            require_collateral: parse_decimal("marginUsed"),
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
            self.account_id, child_order_acceptance_id
        );
        let _ = self.send_json(self.authed(Method::PUT, &path)).await?;
        Ok(())
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
        .unwrap_or("")
        .to_string();
    let instrument = order
        .get("instrument")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Map OANDA state string to ChildOrderState enum.
    // OANDA states: PENDING, FILLED, TRIGGERED, CANCELLED.
    let oanda_state = order
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("PENDING");
    let child_order_state = match oanda_state {
        "FILLED" | "TRIGGERED" => ChildOrderState::Completed,
        "CANCELLED" => ChildOrderState::Canceled,
        _ => ChildOrderState::Active, // PENDING or unknown → treat as active
    };

    let units_str = order.get("units").and_then(|v| v.as_str()).unwrap_or("0");
    let units: i64 = units_str.parse().unwrap_or(0);
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
        executed_size: size,
        total_commission: Decimal::ZERO,
    })
}
