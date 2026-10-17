use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Request body of a child order sent to the exchange.
#[derive(Debug, Clone, Serialize)]
pub struct SendChildOrderRequest {
    /// Product code of the order.
    pub product_code: String,
    /// Whether the order is a market order or a limit order.
    pub child_order_type: ChildOrderType,
    /// Whether the order buys or sells.
    pub side: Side,
    /// Size of the order.
    pub size: Decimal,
    /// Limit price, left out of the body when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Decimal>,
    /// Number of minutes until the order expires, left out of the body when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minute_to_expire: Option<u32>,
    /// Time-in-force condition of the order, left out of the body when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<TimeInForce>,
    /// Identifier of the position that the order closes, or `None` for an order that opens a position; it is never serialized into the body.
    #[serde(skip)]
    pub close_position_id: Option<String>,
}

/// Type of a child order.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChildOrderType {
    /// Order that executes at the market price.
    Market,
    /// Order that executes at its limit price.
    Limit,
}

/// Side of an order.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    /// Buy side.
    Buy,
    /// Sell side.
    Sell,
}

/// Time-in-force condition of an order.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum TimeInForce {
    /// Good till canceled.
    Gtc,
    /// Immediate or cancel.
    Ioc,
    /// Fill or kill.
    Fok,
}

/// Response of the exchange to a sent child order.
#[derive(Debug, Clone, Deserialize)]
pub struct SendChildOrderResponse {
    /// Acceptance ID that the exchange assigned to the order.
    pub child_order_acceptance_id: String,
}

/// Child order as the exchange reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct ChildOrder {
    /// Numeric identifier of the order.
    pub id: u64,
    /// Identifier of the order.
    pub child_order_id: String,
    /// Product code of the order.
    pub product_code: String,
    /// Side of the order as text.
    pub side: String,
    /// Type of the order as text.
    pub child_order_type: String,
    /// Price of the order.
    pub price: Decimal,
    /// Average execution price of the order.
    pub average_price: Decimal,
    /// Size of the order.
    pub size: Decimal,
    /// State of the order.
    pub child_order_state: ChildOrderState,
    /// Expiry date of the order as text.
    pub expire_date: String,
    /// Date of the order as text.
    pub child_order_date: String,
    /// Acceptance ID of the order.
    pub child_order_acceptance_id: String,
    /// Size that remains unexecuted.
    pub outstanding_size: Decimal,
    /// Size that was canceled.
    pub cancel_size: Decimal,
    /// Size that was executed.
    pub executed_size: Decimal,
    /// Total commission of the order.
    pub total_commission: Decimal,
}

/// State of a child order.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChildOrderState {
    /// The order is open.
    Active,
    /// The order is fully executed.
    Completed,
    /// The order was canceled.
    Canceled,
    /// The order expired.
    Expired,
    /// The order was rejected.
    Rejected,
}

/// Execution of a child order as the exchange reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct Execution {
    /// Numeric identifier of the execution.
    pub id: u64,
    /// Identifier of the child order.
    pub child_order_id: String,
    /// Side of the execution as text.
    pub side: String,
    /// Price of the execution.
    pub price: Decimal,
    /// Size of the execution.
    pub size: Decimal,
    /// Commission of the execution.
    pub commission: Decimal,
    /// Date of the execution as text.
    pub exec_date: String,
    /// Acceptance ID of the child order.
    pub child_order_acceptance_id: String,
}

/// Open position as the exchange reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangePosition {
    /// Product code of the position.
    pub product_code: String,
    /// Side of the position as text.
    pub side: String,
    /// Open price of the position.
    pub price: Decimal,
    /// Size of the position.
    pub size: Decimal,
    /// Commission of the position.
    pub commission: Decimal,
    /// Accumulated swap points of the position.
    pub swap_point_accumulate: Decimal,
    /// Collateral that the position requires.
    pub require_collateral: Decimal,
    /// Open date of the position as text.
    pub open_date: String,
    /// Leverage of the position.
    pub leverage: Decimal,
    /// Unrealized profit or loss of the position.
    pub pnl: Decimal,
    /// SFD of the position.
    pub sfd: Decimal,
}

/// Collateral of the account as the exchange reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct Collateral {
    /// Collateral deposited.
    pub collateral: Decimal,
    /// Unrealized profit or loss of the open positions.
    pub open_position_pnl: Decimal,
    /// Collateral that the open positions require.
    pub require_collateral: Decimal,
    /// Ratio of the collateral to the required collateral.
    pub keep_rate: Decimal,
}

/// Error body that the exchange returns.
#[derive(Debug, Clone, Deserialize)]
pub struct BitflyerErrorBody {
    /// Error status code.
    pub status: i32,
    /// Error message.
    pub error_message: String,
    /// Additional data of the error.
    #[serde(default)]
    pub data: serde_json::Value,
}

/// Error of a call to the bitFlyer private API.
#[derive(Debug, Error)]
pub enum BitflyerApiError {
    /// The account has insufficient funds, with the message of the exchange.
    #[error("invalid order: insufficient funds: {0}")]
    InsufficientFunds(String),
    /// The API key is invalid.
    #[error("invalid api key")]
    InvalidApiKey,
    /// The signature of the request is invalid.
    #[error("invalid signature")]
    InvalidSignature,
    /// The exchange limited the request rate.
    #[error("failed to send request: rate limited, retry after {retry_after:?}")]
    RateLimited {
        /// Delay that the exchange asked for, when it gave one.
        retry_after: Option<std::time::Duration>,
    },
    /// The order was not found, with the message of the exchange.
    #[error("invalid order: not found: {0}")]
    OrderNotFound(String),
    /// The exchange reported an error status.
    #[error("failed to call bitflyer api: status={status} message={message}")]
    ApiError {
        /// Error status code.
        status: i32,
        /// Error message of the exchange.
        message: String,
    },
    /// The HTTP request failed.
    #[error("failed to send http request: {0}")]
    Http(reqwest::Error),
    /// The response is invalid, with the cause.
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

impl From<reqwest::Error> for BitflyerApiError {
    fn from(e: reqwest::Error) -> Self {
        BitflyerApiError::Http(e.without_url())
    }
}

impl BitflyerApiError {
    /// Maps an error body to the error variant that its status denotes: `-200` and `-205` to `InsufficientFunds`, `-201` to `InvalidApiKey`, `-207` to `InvalidSignature`, `-208` to `OrderNotFound` and any other status to `ApiError`.
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
