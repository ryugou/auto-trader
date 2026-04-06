use super::{ApiError, AppState};
use crate::api::filters::TradeFilter;
use auto_trader_db::dashboard::{self, TradeRow};
use axum::extract::{Query, State};
use axum::Json;
use serde::Serialize;

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
        filter.paper_account_id,
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
