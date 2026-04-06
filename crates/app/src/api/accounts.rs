use super::{ApiError, AppState};
use auto_trader_db::dashboard;
use auto_trader_db::paper_accounts::{
    self, CreatePaperAccount, PaperAccount, UpdatePaperAccount,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct PaperAccountWithBalance {
    pub id: Uuid,
    pub name: String,
    pub exchange: String,
    pub initial_balance: Decimal,
    pub current_balance: Decimal,
    pub currency: String,
    pub leverage: Decimal,
    pub strategy: String,
    pub account_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub unrealized_pnl: Decimal,
    pub evaluated_balance: Decimal,
}

impl PaperAccountWithBalance {
    fn new(account: PaperAccount, unrealized_pnl: Decimal, evaluated_balance: Decimal) -> Self {
        Self {
            id: account.id,
            name: account.name,
            exchange: account.exchange,
            initial_balance: account.initial_balance,
            current_balance: account.current_balance,
            currency: account.currency,
            leverage: account.leverage,
            strategy: account.strategy,
            account_type: account.account_type,
            created_at: account.created_at,
            updated_at: account.updated_at,
            unrealized_pnl,
            evaluated_balance,
        }
    }
}

pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<Vec<PaperAccountWithBalance>>, ApiError> {
    let accounts = paper_accounts::list_paper_accounts(&state.pool)
        .await
        .map_err(ApiError::from)?;

    let mut enriched = Vec::with_capacity(accounts.len());
    for account in accounts {
        let eval = dashboard::get_evaluated_balance(&state.pool, account.id)
            .await
            .map_err(ApiError::from)?;
        enriched.push(PaperAccountWithBalance::new(
            account,
            eval.unrealized_pnl,
            eval.evaluated_balance,
        ));
    }
    Ok(Json(enriched))
}

pub async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<PaperAccountWithBalance>, ApiError> {
    let account = paper_accounts::get_paper_account(&state.pool, id)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "account not found".to_string(),
        ))?;
    let eval = dashboard::get_evaluated_balance(&state.pool, id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(PaperAccountWithBalance::new(
        account,
        eval.unrealized_pnl,
        eval.evaluated_balance,
    )))
}

pub async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreatePaperAccount>,
) -> Result<impl IntoResponse, ApiError> {
    if req.account_type != "paper" && req.account_type != "live" {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "account_type must be 'paper' or 'live'".to_string(),
        ));
    }
    paper_accounts::create_paper_account(&state.pool, &req)
        .await
        .map(|a| (StatusCode::CREATED, Json(a)))
        .map_err(Into::into)
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePaperAccount>,
) -> Result<Json<PaperAccount>, ApiError> {
    paper_accounts::update_paper_account(&state.pool, id, &req)
        .await
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "account not found".to_string(),
        ))
}

pub async fn remove(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let deleted = paper_accounts::delete_paper_account(&state.pool, id)
        .await
        .map_err(ApiError::from)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(
            StatusCode::NOT_FOUND,
            "account not found".to_string(),
        ))
    }
}
