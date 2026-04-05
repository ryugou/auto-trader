use auto_trader_db::paper_accounts::{
    self, CreatePaperAccount, PaperAccount, UpdatePaperAccount,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

pub fn router(pool: PgPool) -> Router {
    Router::new()
        .route("/api/paper-accounts", get(list).post(create))
        .route(
            "/api/paper-accounts/{id}",
            get(get_one).put(update).delete(remove),
        )
        .with_state(pool)
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let msg = e.to_string();
        if msg.contains("duplicate key") || msg.contains("unique constraint") {
            ApiError(StatusCode::CONFLICT, msg)
        } else if msg.contains("foreign key") || msg.contains("violates foreign key") {
            ApiError(StatusCode::CONFLICT, "account has related trades, cannot delete".to_string())
        } else {
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg)
        }
    }
}

async fn list(State(pool): State<PgPool>) -> Result<Json<Vec<PaperAccount>>, ApiError> {
    paper_accounts::list_paper_accounts(&pool)
        .await
        .map(Json)
        .map_err(Into::into)
}

async fn get_one(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<PaperAccount>, ApiError> {
    paper_accounts::get_paper_account(&pool, id)
        .await
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "account not found".to_string()))
}

async fn create(
    State(pool): State<PgPool>,
    Json(req): Json<CreatePaperAccount>,
) -> Result<impl IntoResponse, ApiError> {
    paper_accounts::create_paper_account(&pool, &req)
        .await
        .map(|a| (StatusCode::CREATED, Json(a)))
        .map_err(Into::into)
}

async fn update(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePaperAccount>,
) -> Result<Json<PaperAccount>, ApiError> {
    paper_accounts::update_paper_account(&pool, id, &req)
        .await
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "account not found".to_string()))
}

async fn remove(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let deleted = paper_accounts::delete_paper_account(&pool, id)
        .await
        .map_err(ApiError::from)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(StatusCode::NOT_FOUND, "account not found".to_string()))
    }
}
