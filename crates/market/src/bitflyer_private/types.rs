use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

#[cfg(test)]
mod tests;
