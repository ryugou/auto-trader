use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::bitflyer_private::Side;

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

pub(crate) fn side_to_gmo(side: Side) -> GmoSide {
    match side {
        Side::Buy => GmoSide::Buy,
        Side::Sell => GmoSide::Sell,
    }
}

#[cfg(test)]
mod tests;
