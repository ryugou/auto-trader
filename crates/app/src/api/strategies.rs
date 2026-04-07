use super::{ApiError, AppState};
use auto_trader_db::strategies::{self, Strategy};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Optional `fx` / `crypto` filter to scope the dropdown by exchange.
    pub category: Option<String>,
}

/// `GET /api/strategies` — read-only catalog list. Optional `?category=`.
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<Strategy>>, ApiError> {
    let category = query.category.as_deref();
    let rows = strategies::list_strategies(&state.pool, category)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(rows))
}

/// `GET /api/strategies/{name}` — single-strategy lookup for the catalog
/// detail view.
pub async fn get_one(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Strategy>, ApiError> {
    strategies::get_strategy(&state.pool, &name)
        .await
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            format!("strategy '{name}' not found"),
        ))
}
