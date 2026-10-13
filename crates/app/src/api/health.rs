use super::{ApiError, AppState};
use crate::price_store::FeedHealth;
use axum::Json;
use axum::extract::State;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MarketFeedHealthResponse {
    pub feeds: Vec<FeedHealth>,
}

pub async fn market_feed(
    State(state): State<AppState>,
) -> Result<Json<MarketFeedHealthResponse>, ApiError> {
    let feeds = state.price_store.health_at(chrono::Utc::now()).await;
    Ok(Json(MarketFeedHealthResponse { feeds }))
}
