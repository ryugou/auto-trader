mod accounts;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use sqlx::PgPool;

pub fn router(pool: PgPool) -> Router {
    let api_token = std::env::var("API_TOKEN").ok();
    Router::new()
        .route("/api/paper-accounts", get(accounts::list).post(accounts::create))
        .route(
            "/api/paper-accounts/{id}",
            get(accounts::get_one).put(accounts::update).delete(accounts::remove),
        )
        .layer(middleware::from_fn(move |req, next| {
            let token = api_token.clone();
            auth_middleware(token, req, next)
        }))
        .with_state(pool)
}

async fn auth_middleware(
    api_token: Option<String>,
    req: Request,
    next: Next,
) -> axum::response::Response {
    if let Some(expected) = &api_token {
        let auth = req.headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match auth {
            Some(token) if token == expected => next.run(req).await,
            _ => (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" }))).into_response(),
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
                return match pg_err.code().as_deref() {
                    Some("23505") => ApiError(StatusCode::CONFLICT, "duplicate name".to_string()),
                    Some("23503") => ApiError(StatusCode::CONFLICT, "account has related trades, cannot delete".to_string()),
                    _ => ApiError(StatusCode::INTERNAL_SERVER_ERROR, "database error".to_string()),
                };
            }
        }
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
    }
}
