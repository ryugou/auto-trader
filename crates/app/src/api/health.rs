use super::{ApiError, AppState};
use crate::price_store::FeedHealth;
use axum::extract::State;
use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MarketFeedHealthResponse {
    pub feeds: Vec<FeedHealth>,
}

/// `GET /api/health/market-feed` — rollup of every expected feed
/// against its observed last tick. The status is one of:
///   - `healthy`: a tick is present and not older than 60 seconds
///   - `stale`: a tick is present but older than 60 seconds
///   - `missing`: no tick has been received since process start
///
/// Feeds the operator did NOT configure (e.g. OANDA when the API
/// key is unset) are absent from the response, so the dashboard
/// banner does not raise false alarms.
pub async fn market_feed(
    State(state): State<AppState>,
) -> Result<Json<MarketFeedHealthResponse>, ApiError> {
    let feeds = state.price_store.health_at(chrono::Utc::now()).await;
    Ok(Json(MarketFeedHealthResponse { feeds }))
}
