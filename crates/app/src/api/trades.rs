use super::{ApiError, AppState};
use crate::api::filters::TradeFilter;
use auto_trader_db::dashboard::{self, TradeRow};
use auto_trader_db::trades::{self as trades_db, TradeEvent};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct TradesResponse {
    pub trades: Vec<TradeRow>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

pub async fn list(
    State(state): State<AppState>,
    Query(filter): Query<TradeFilter>,
) -> Result<Json<TradesResponse>, ApiError> {
    let page = filter.page.unwrap_or(1).max(1);
    let per_page = filter.per_page.unwrap_or(50).min(200);

    let (trades, total) = dashboard::get_trades(
        &state.pool,
        filter.exchange.as_deref(),
        filter.account_id,
        filter.strategy.as_deref(),
        filter.pair.as_deref(),
        filter.status.as_deref(),
        Some(page),
        Some(per_page),
    )
    .await
    .map_err(ApiError::from)?;

    Ok(Json(TradesResponse {
        trades,
        total,
        page,
        per_page,
    }))
}

#[derive(Debug, Serialize)]
pub struct TradeEventsResponse {
    pub events: Vec<TradeEvent>,
}

/// `GET /api/trades/{id}/events` — return the chronological event
/// timeline (OPEN → overnight fees → CLOSE) for a single trade. Used
/// by the trade-history table's expandable row.
pub async fn events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TradeEventsResponse>, ApiError> {
    let events = trades_db::get_trade_events(&state.pool, id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "trade not found".to_string()))?;
    Ok(Json(TradeEventsResponse { events }))
}
