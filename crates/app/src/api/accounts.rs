use super::{ApiError, AppState};
use auto_trader_core::types::Exchange;
use auto_trader_db::dashboard;
use auto_trader_db::strategies;
use auto_trader_db::trading_accounts::{
    self, CreateTradingAccount, TradingAccount, UpdateTradingAccount, normalize_currency,
    validate_initial_balance,
};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct AccountWithBalance {
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
    pub unrealized_pnl: Decimal,
    pub evaluated_balance: Decimal,
}

impl AccountWithBalance {
    fn new(account: TradingAccount, unrealized_pnl: Decimal, evaluated_balance: Decimal) -> Self {
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
            unrealized_pnl,
            evaluated_balance,
        }
    }
}

pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<Vec<AccountWithBalance>>, ApiError> {
    let accounts = trading_accounts::list_all(&state.pool)
        .await
        .map_err(ApiError::from)?;

    // Single query for all account balances (no N+1).
    let balances = dashboard::get_all_evaluated_balances(&state.pool)
        .await
        .map_err(ApiError::from)?;

    let enriched = accounts
        .into_iter()
        .map(|account| {
            let eval = balances.get(&account.id);
            let unrealized_pnl = eval.map_or(Decimal::ZERO, |e| e.unrealized_pnl);
            let evaluated_balance = eval.map_or(account.current_balance, |e| e.evaluated_balance);
            AccountWithBalance::new(account, unrealized_pnl, evaluated_balance)
        })
        .collect();
    Ok(Json(enriched))
}

pub async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AccountWithBalance>, ApiError> {
    let account = trading_accounts::get_account(&state.pool, id)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "account not found".to_string(),
        ))?;
    let eval = dashboard::get_evaluated_balance(&state.pool, id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(AccountWithBalance::new(
        account,
        eval.unrealized_pnl,
        eval.evaluated_balance,
    )))
}

pub async fn create(
    State(state): State<AppState>,
    mut req: Json<CreateTradingAccount>,
) -> Result<impl IntoResponse, ApiError> {
    // Early validation for user-friendly 400 errors. DB CHECK constraint
    // also enforces this, but would surface as a less clear 500 / constraint
    // error. API validates for UX; DB validates for integrity.
    if req.account_type != "paper" && req.account_type != "live" {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "account_type must be 'paper' or 'live'".to_string(),
        ));
    }
    req.currency = normalize_currency(&req.currency);
    if let Err(msg) = validate_initial_balance(&req.currency, req.initial_balance) {
        return Err(ApiError(StatusCode::BAD_REQUEST, msg));
    }
    // Validate exchange is a known enum value (returns 400 instead of letting
    // the DB CHECK constraint surface as 500).
    let exchange_normalized = req.exchange.trim().to_ascii_lowercase();
    let exchange_enum: Exchange = match exchange_normalized.parse::<Exchange>() {
        Ok(e) => e,
        Err(e) => return Err(ApiError(StatusCode::BAD_REQUEST, e.to_string())),
    };
    // Defense in depth: reject creation on exchanges that have no
    // [exchange_margin.<name>] entry. Without this, the worker tasks
    // (executor / monitor / exit) would log an error and skip every
    // signal/close on this account because PositionSizer cannot compute
    // a quantity without `liquidation_margin_level`. Failing the create
    // here surfaces the configuration gap to the operator immediately
    // instead of silently dropping trades after the account is in use.
    if !state.exchange_liquidation_levels.contains_key(&exchange_enum) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!(
                "exchange '{}' has no [exchange_margin.{}] entry in config; \
                 add `liquidation_margin_level` and restart the service before \
                 creating accounts on this exchange",
                exchange_normalized, exchange_normalized
            ),
        ));
    }
    if !strategies::strategy_exists(&state.pool, &req.strategy)
        .await
        .map_err(ApiError::from)?
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!(
                "strategy '{}' not found in catalog (see GET /api/strategies)",
                req.strategy
            ),
        ));
    }
    // Duplicate live account per exchange: the DB partial unique index is
    // the real guard, but pre-check here for a friendly 409.
    if req.account_type == "live" {
        let existing: Option<(uuid::Uuid,)> = sqlx::query_as(
            "SELECT id FROM trading_accounts WHERE exchange = $1 AND account_type = 'live' LIMIT 1",
        )
        .bind(&exchange_normalized)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
        if existing.is_some() {
            return Err(ApiError(
                StatusCode::CONFLICT,
                format!(
                    "live account for exchange '{}' already exists; only 1 live account per exchange is supported",
                    exchange_normalized
                ),
            ));
        }
    }
    trading_accounts::create_account(&state.pool, &req)
        .await
        .map(|a| (StatusCode::CREATED, Json(a)))
        .map_err(Into::into)
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTradingAccount>,
) -> Result<Json<TradingAccount>, ApiError> {
    if let Some(name) = req.strategy.as_deref()
        && !strategies::strategy_exists(&state.pool, name)
            .await
            .map_err(ApiError::from)?
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!("strategy '{name}' not found in catalog (see GET /api/strategies)"),
        ));
    }
    trading_accounts::update_account(&state.pool, id, &req)
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
    let deleted = trading_accounts::delete_account(&state.pool, id)
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
