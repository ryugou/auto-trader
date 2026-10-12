use rust_decimal::Decimal;

#[derive(serde::Deserialize)]
pub(super) struct JsonRpcMessage {
    pub(super) method: Option<String>,
    pub(super) params: Option<TickerParams>,
}

#[derive(serde::Deserialize)]
pub(super) struct TickerParams {
    pub(super) message: TickerMessage,
}

#[derive(serde::Deserialize)]
pub(super) struct TickerMessage {
    pub(super) product_code: String,
    pub(super) best_bid: Decimal,
    pub(super) best_ask: Decimal,
    pub(super) ltp: Decimal,
    pub(super) volume: Decimal,
    pub(super) timestamp: String,
}
