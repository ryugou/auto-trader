use super::{ApiError, AppState};
use auto_trader_db::notifications::{self as notifs_db, Notification};
use axum::extract::{Query, State};
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Default)]
pub struct NotificationsFilter {
    pub unread_only: Option<bool>,
    pub limit: Option<i64>,
    pub page: Option<i64>,
    pub kind: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NotificationsResponse {
    pub items: Vec<Notification>,
    pub total: i64,
    pub unread_count: i64,
    pub page: i64,
    pub limit: i64,
}

#[derive(Debug, Serialize)]
pub struct UnreadCountResponse {
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct MarkReadResponse {
    pub marked: u64,
}

/// Parse an optional `YYYY-MM-DD` query parameter. An absent or empty
/// string yields `Ok(None)` so callers can clear the filter; a present
/// but malformed value yields `Err(400)` so the user gets a clear
/// signal instead of "you asked for a filter and silently got the
/// unfiltered result set".
fn parse_opt_date(s: Option<&str>, field: &str) -> Result<Option<NaiveDate>, ApiError> {
    match s {
        None | Some("") => Ok(None),
        Some(v) => NaiveDate::parse_from_str(v, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| {
                ApiError(
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("invalid {field} (expected YYYY-MM-DD)"),
                )
            }),
    }
}

pub async fn list(
    State(state): State<AppState>,
    Query(filter): Query<NotificationsFilter>,
) -> Result<Json<NotificationsResponse>, ApiError> {
    let page = filter.page.unwrap_or(1).max(1);
    let limit = filter.limit.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * limit;
    let unread_only = filter.unread_only.unwrap_or(false);
    let from = parse_opt_date(filter.from.as_deref(), "from")?;
    let to = parse_opt_date(filter.to.as_deref(), "to")?;

    // kind must be one of the two known values or None — reject
    // anything else so a typo can't silently collapse to "no match".
    let kind = match filter.kind.as_deref() {
        None | Some("") => None,
        Some(k @ "trade_opened") | Some(k @ "trade_closed") => Some(k),
        Some(_) => {
            return Err(ApiError(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid kind (expected trade_opened | trade_closed)".to_string(),
            ));
        }
    };

    let (items, total) =
        notifs_db::list(&state.pool, limit, offset, unread_only, kind, from, to)
            .await
            .map_err(ApiError::from)?;
    let unread_count = notifs_db::unread_count(&state.pool)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(NotificationsResponse {
        items,
        total,
        unread_count,
        page,
        limit,
    }))
}

pub async fn unread_count(
    State(state): State<AppState>,
) -> Result<Json<UnreadCountResponse>, ApiError> {
    let count = notifs_db::unread_count(&state.pool)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(UnreadCountResponse { count }))
}

pub async fn mark_all_read(
    State(state): State<AppState>,
) -> Result<Json<MarkReadResponse>, ApiError> {
    let marked = notifs_db::mark_all_read(&state.pool)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(MarkReadResponse { marked }))
}
