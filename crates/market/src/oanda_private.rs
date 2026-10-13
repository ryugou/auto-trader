use std::str::FromStr;

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};
use rust_decimal::Decimal;

use crate::bitflyer_private::{
    ChildOrder, ChildOrderState, ChildOrderType, Collateral, ExchangePosition, Execution,
    MAX_ERROR_BODY_CHARS, SendChildOrderRequest, SendChildOrderResponse, truncate_body,
};
use crate::exchange_api::ExchangeApi;

fn encode_path(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

#[derive(Debug, thiserror::Error)]
pub enum OandaApiError {
    #[error("failed to send http request: {0}")]
    Http(reqwest::Error),
    #[error("failed to call oanda api: status={status} body={body}")]
    Api { status: StatusCode, body: String },
    #[error("failed to parse response body: {0}")]
    Parse(String),
    #[error("invalid config: {0}")]
    Config(String),
}

impl From<reqwest::Error> for OandaApiError {
    fn from(e: reqwest::Error) -> Self {
        OandaApiError::Http(e.without_url())
    }
}

pub struct OandaPrivateApi {
    client: Client,
    base_url: String,
    account_id: String,
    api_key: String,
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

    fn sanitize_body_snippet(&self, body: &str) -> String {
        let redacted = if body.contains(&self.account_id) {
            body.replace(&self.account_id, "<REDACTED_ACCOUNT_ID>")
        } else {
            body.to_string()
        };
        let truncated = truncate_body(&redacted);
        if redacted.chars().count() > MAX_ERROR_BODY_CHARS {
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
        if !matches!(req.child_order_type, ChildOrderType::Market) {
            anyhow::bail!("unsupported order type: LIMIT");
        }

        use crate::bitflyer_private::Side;
        use rust_decimal::prelude::ToPrimitive;

        if req.size.is_sign_negative() || req.size.is_zero() {
            anyhow::bail!("invalid size: must be positive non-zero");
        }
        if req.size.fract() != rust_decimal::Decimal::ZERO {
            anyhow::bail!("invalid size: must be an integer number of units");
        }
        let units_abs: i64 = req
            .size
            .to_i64()
            .ok_or_else(|| anyhow::anyhow!("invalid size: does not fit in i64"))?;
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
        } else if matches!(state, "CANCELLED" | "REJECTED" | "EXPIRED") {
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
                "invalid order {child_order_acceptance_id}: reached terminal state {state}{suffix}"
            );
        } else {
            return Ok(Vec::new());
        }
        let fill_ids: Vec<String> = {
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
                id: 0,
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
                .map_err(|e| OandaApiError::Parse(format!("invalid units: {e}")))?;
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
                swap_point_accumulate: Decimal::ZERO,
                require_collateral: Decimal::ZERO,
                open_date: String::new(),
                leverage: Decimal::ZERO,
                pnl: Decimal::ZERO,
                sfd: Decimal::ZERO,
            });
        }
        Ok(out)
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
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
            keep_rate: Decimal::ZERO,
        })
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
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

    let oanda_state = order
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("PENDING");
    let child_order_state = match oanda_state {
        "FILLED" => ChildOrderState::Completed,
        "CANCELLED" => ChildOrderState::Canceled,
        "EXPIRED" => ChildOrderState::Expired,
        "REJECTED" => ChildOrderState::Rejected,
        "TRIGGERED" | "PENDING" => ChildOrderState::Active,
        _ => ChildOrderState::Active,
    };

    let units_str = order
        .get("units")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OandaApiError::Parse(format!("order missing 'units': {order}")))?;
    let units: i64 = units_str
        .parse()
        .map_err(|e| OandaApiError::Parse(format!("invalid units: {e}")))?;
    let side = if units >= 0 { "BUY" } else { "SELL" }.to_string();
    let size = Decimal::from(units.unsigned_abs());

    Ok(ChildOrder {
        id: 0,
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
mod tests;
