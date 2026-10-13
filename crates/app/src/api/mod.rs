mod accounts;
mod dashboard;
pub(crate) mod filters;
mod health;
mod market;
mod notifications;
mod positions;
mod strategies;
mod trades;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub price_store: std::sync::Arc<crate::price_store::PriceStore>,
    pub exchange_liquidation_levels: std::sync::Arc<
        std::collections::HashMap<auto_trader_core::types::Exchange, rust_decimal::Decimal>,
    >,
}

pub fn router(state: AppState) -> Router {
    let api_token = std::env::var("API_TOKEN").ok();

    let api_routes = Router::new()
        .route(
            "/trading-accounts",
            get(accounts::list).post(accounts::create),
        )
        .route(
            "/trading-accounts/:id",
            get(accounts::get_one)
                .put(accounts::update)
                .delete(accounts::remove),
        )
        .route("/dashboard/summary", get(dashboard::summary))
        .route("/dashboard/pnl-history", get(dashboard::pnl_history))
        .route(
            "/dashboard/balance-history",
            get(dashboard::balance_history),
        )
        .route("/dashboard/strategies", get(dashboard::strategies))
        .route("/dashboard/pairs", get(dashboard::pairs))
        .route("/dashboard/hourly-winrate", get(dashboard::hourly_winrate))
        .route("/trades", get(trades::list))
        .route("/trades/:id/events", get(trades::events))
        .route("/positions", get(positions::list))
        .route("/strategies", get(strategies::list))
        .route("/strategies/:name", get(strategies::get_one))
        .route("/notifications", get(notifications::list))
        .route(
            "/notifications/unread-count",
            get(notifications::unread_count),
        )
        .route(
            "/notifications/mark-all-read",
            axum::routing::post(notifications::mark_all_read),
        )
        .route("/market/prices", get(market::prices))
        .route("/health/market-feed", get(health::market_feed))
        .layer(middleware::from_fn(move |req, next| {
            let token = api_token.clone();
            auth_middleware(token, req, next)
        }))
        .with_state(state);

    Router::new()
        .nest("/api", api_routes)
        .fallback_service(
            ServeDir::new("dashboard-ui/dist")
                .fallback(ServeFile::new("dashboard-ui/dist/index.html")),
        )
        .layer(build_permissive_cors_for_same_host_dashboard())
}

fn build_permissive_cors_for_same_host_dashboard() -> CorsLayer {
    CorsLayer::permissive()
}

async fn auth_middleware(
    api_token: Option<String>,
    req: Request,
    next: Next,
) -> axum::response::Response {
    if let Some(expected) = &api_token {
        let auth = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match auth {
            Some(token) if token == expected => next.run(req).await,
            _ => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "unauthorized" })),
            )
                .into_response(),
        }
    } else {
        next.run(req).await
    }
}

pub(crate) struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        for cause in e.chain() {
            if let Some(sqlx::Error::Database(pg_err)) = cause.downcast_ref::<sqlx::Error>() {
                return map_database_error_to_api_error(&e, pg_err.as_ref());
            }
        }
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal error".to_string(),
        )
    }
}

fn map_database_error_to_api_error(
    e: &anyhow::Error,
    pg_err: &(dyn sqlx::error::DatabaseError + 'static),
) -> ApiError {
    match pg_err.code().as_deref() {
        Some("23505") => map_unique_violation_to_api_error(e, pg_err),
        Some("23503") => map_foreign_key_violation_to_api_error(pg_err),
        _ => ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error".to_string(),
        ),
    }
}

fn map_unique_violation_to_api_error(
    e: &anyhow::Error,
    pg_err: &(dyn sqlx::error::DatabaseError + 'static),
) -> ApiError {
    match pg_err.constraint() {
        Some("trading_accounts_one_live_per_exchange") => {
            ApiError(StatusCode::CONFLICT, e.to_string())
        }
        _ => ApiError(StatusCode::CONFLICT, "invalid name: duplicate".to_string()),
    }
}

fn map_foreign_key_violation_to_api_error(
    pg_err: &(dyn sqlx::error::DatabaseError + 'static),
) -> ApiError {
    match pg_err.constraint() {
        Some("trading_accounts_strategy_fkey") => ApiError(
            StatusCode::BAD_REQUEST,
            "invalid strategy: not found in catalog (see GET /api/strategies)".to_string(),
        ),
        _ => ApiError(
            StatusCode::CONFLICT,
            "failed to delete account: related trades exist".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests;
