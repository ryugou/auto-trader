use super::{ApiError, AppState};
use axum::Json;
use axum::extract::State;
use rust_decimal::Decimal;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MarketPrice {
    pub exchange: String,
    pub pair: String,
    pub price: Decimal,
    pub ts: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct MarketPricesResponse {
    pub prices: Vec<MarketPrice>,
}

/// `GET /api/market/prices` — current snapshot of every (exchange,
/// pair) tuple that has at least one observed tick. Used by the
/// Positions page to compute unrealized P&L. Paging and large
/// historical pulls are intentionally not supported here — this
/// endpoint exists to be cheap and frequently polled.
pub async fn prices(State(state): State<AppState>) -> Result<Json<MarketPricesResponse>, ApiError> {
    let snapshot = state.price_store.snapshot().await;
    let prices = snapshot
        .into_iter()
        .map(|(key, tick)| MarketPrice {
            exchange: key.exchange.as_str().to_string(),
            pair: key.pair.0,
            price: tick.price,
            ts: tick.ts,
        })
        .collect();
    Ok(Json(MarketPricesResponse { prices }))
}
