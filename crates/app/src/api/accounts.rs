use super::ApiError;
use auto_trader_db::paper_accounts::{
    self, CreatePaperAccount, PaperAccount, UpdatePaperAccount,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn list(State(pool): State<PgPool>) -> Result<Json<Vec<PaperAccount>>, ApiError> {
    paper_accounts::list_paper_accounts(&pool)
        .await
        .map(Json)
        .map_err(Into::into)
}

pub async fn get_one(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<PaperAccount>, ApiError> {
    paper_accounts::get_paper_account(&pool, id)
        .await
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "account not found".to_string()))
}

pub async fn create(
    State(pool): State<PgPool>,
    Json(req): Json<CreatePaperAccount>,
) -> Result<impl IntoResponse, ApiError> {
    paper_accounts::create_paper_account(&pool, &req)
        .await
        .map(|a| (StatusCode::CREATED, Json(a)))
        .map_err(Into::into)
}

pub async fn update(
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

pub async fn remove(
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
