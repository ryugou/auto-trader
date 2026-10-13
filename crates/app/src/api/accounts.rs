use std::collections::HashMap;

use auto_trader_core::types::Exchange;
use auto_trader_db::dashboard;
use auto_trader_db::strategies;
use auto_trader_db::trades;
use auto_trader_db::trading_accounts::{
    self, CreateTradingAccount, TradingAccount, UpdateTradingAccount, normalize_currency,
    validate_initial_balance,
};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

use super::{ApiError, AppState};
use crate::api::filters::AccountsFilter;

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
    pub active: bool,
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
            active: account.active,
            unrealized_pnl,
            evaluated_balance,
        }
    }
}

pub async fn list(
    State(state): State<AppState>,
    Query(filter): Query<AccountsFilter>,
) -> Result<Json<Vec<AccountWithBalance>>, ApiError> {
    let accounts = if filter.include_inactive.unwrap_or(false) {
        trading_accounts::list_all(&state.pool).await
    } else {
        trading_accounts::list_active(&state.pool).await
    }
    .map_err(ApiError::from)?;

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
            "invalid account: not found".to_string(),
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

fn reject_account_activation_with_unresolved_liquidation_level(
    levels: &HashMap<Exchange, Decimal>,
    exchange_enum: Exchange,
    exchange_normalized: &str,
) -> Result<(), ApiError> {
    match levels.get(&exchange_enum) {
        None => Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!(
                "invalid option \"exchange_margin.{exchange_normalized}\": missing; add \
                 `liquidation_margin_level` and restart the service before creating accounts \
                 on this exchange"
            ),
        )),
        Some(y) if *y <= Decimal::ZERO => Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!(
                "invalid option \"exchange_margin.{exchange_normalized}.liquidation_margin_level\": \
                 must be > 0, got {y}; fix the value in config and restart the service before \
                 creating accounts on this exchange"
            ),
        )),
        Some(_) => Ok(()),
    }
}

async fn find_active_live_account_for_exchange(
    pool: &sqlx::PgPool,
    exchange: &str,
    excluding_id: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    let existing: Option<(Uuid,)> = match excluding_id {
        Some(id) => {
            sqlx::query_as(
                "SELECT id FROM trading_accounts
                 WHERE exchange = $1 AND account_type = 'live' AND active AND id <> $2
                 LIMIT 1",
            )
            .bind(exchange)
            .bind(id)
            .fetch_optional(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id FROM trading_accounts
                 WHERE exchange = $1 AND account_type = 'live' AND active
                 LIMIT 1",
            )
            .bind(exchange)
            .fetch_optional(pool)
            .await
        }
    }
    .map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
    Ok(existing.map(|(id,)| id))
}

pub async fn create(
    State(state): State<AppState>,
    mut req: Json<CreateTradingAccount>,
) -> Result<impl IntoResponse, ApiError> {
    if req.account_type != "paper" && req.account_type != "live" {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid account_type: must be 'paper' or 'live'".to_string(),
        ));
    }
    req.currency = normalize_currency(&req.currency);
    if let Err(msg) = validate_initial_balance(&req.currency, req.initial_balance) {
        return Err(ApiError(StatusCode::BAD_REQUEST, msg));
    }
    let exchange_normalized = req.exchange.trim().to_ascii_lowercase();
    let exchange_enum: Exchange = match exchange_normalized.parse::<Exchange>() {
        Ok(e) => e,
        Err(e) => return Err(ApiError(StatusCode::BAD_REQUEST, e.to_string())),
    };
    if let Err(msg) = trading_accounts::validate_leverage_for_exchange(exchange_enum, req.leverage)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, msg));
    }
    reject_account_activation_with_unresolved_liquidation_level(
        &state.exchange_liquidation_levels,
        exchange_enum,
        &exchange_normalized,
    )?;
    if !strategies::strategy_exists(&state.pool, &req.strategy)
        .await
        .map_err(ApiError::from)?
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid strategy: not found in catalog (see GET /api/strategies)".to_string(),
        ));
    }
    if req.account_type == "live"
        && find_active_live_account_for_exchange(&state.pool, &exchange_normalized, None)
            .await?
            .is_some()
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            trading_accounts::build_one_live_per_exchange_conflict_message(
                trading_accounts::CREATE_ACCOUNT_OPERATION,
                &exchange_normalized,
            ),
        ));
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
            "invalid strategy: not found in catalog (see GET /api/strategies)".to_string(),
        ));
    }
    if let Some(new_leverage) = req.leverage
        && let Some(account) = trading_accounts::get(&state.pool, id)
            .await
            .map_err(ApiError::from)?
    {
        let exchange_enum: Exchange = account
            .exchange
            .parse()
            .map_err(|e: anyhow::Error| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
        if let Err(msg) =
            trading_accounts::validate_leverage_for_exchange(exchange_enum, new_leverage)
        {
            return Err(ApiError(StatusCode::BAD_REQUEST, msg));
        }
    }
    if req.active == Some(false) {
        reject_deactivation_while_trades_are_unsettled(&state.pool, id).await?;
    }
    if req.active == Some(true) {
        reject_reactivation_violating_activation_guards(&state, id).await?;
    }
    trading_accounts::update_account(&state.pool, id, &req)
        .await
        .map_err(|e| update_account_error_to_api_error(id, e))?
        .map(Json)
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "invalid account: not found".to_string(),
        ))
}

fn build_unsettled_trades_conflict_message(count: usize) -> String {
    format!("failed to deactivate account: {count} open/closing trade(s) exist; close them first")
}

async fn reject_deactivation_while_trades_are_unsettled(
    pool: &sqlx::PgPool,
    id: Uuid,
) -> Result<(), ApiError> {
    let Some(account) = trading_accounts::get(pool, id)
        .await
        .map_err(ApiError::from)?
    else {
        return Ok(());
    };
    if !account.active {
        return Ok(());
    }
    let unsettled = trades::list_open_or_closing_by_account(pool, id)
        .await
        .map_err(ApiError::from)?;
    if !unsettled.is_empty() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            build_unsettled_trades_conflict_message(unsettled.len()),
        ));
    }
    Ok(())
}

async fn reject_reactivation_violating_activation_guards(
    state: &AppState,
    id: Uuid,
) -> Result<(), ApiError> {
    let Some(account) = trading_accounts::get(&state.pool, id)
        .await
        .map_err(ApiError::from)?
    else {
        return Ok(());
    };
    let exchange_enum: Exchange = account
        .exchange
        .parse()
        .map_err(|e: anyhow::Error| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    reject_account_activation_with_unresolved_liquidation_level(
        &state.exchange_liquidation_levels,
        exchange_enum,
        &account.exchange,
    )?;
    if account.account_type == "live"
        && find_active_live_account_for_exchange(&state.pool, &account.exchange, Some(id))
            .await?
            .is_some()
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            trading_accounts::build_one_live_per_exchange_conflict_message(
                trading_accounts::REACTIVATE_ACCOUNT_OPERATION,
                &account.exchange,
            ),
        ));
    }
    Ok(())
}

fn update_account_error_to_api_error(
    id: Uuid,
    e: trading_accounts::UpdateAccountError,
) -> ApiError {
    match e {
        trading_accounts::UpdateAccountError::OpenTrades { count, .. } => ApiError(
            StatusCode::CONFLICT,
            build_unsettled_trades_conflict_message(count),
        ),
        trading_accounts::UpdateAccountError::Other(err) => {
            let message_including_full_error_chain = format!("{err:#}");
            let api_err = ApiError::from(err);
            if api_err.0 == StatusCode::INTERNAL_SERVER_ERROR {
                tracing::error!(
                    account_id = %id,
                    error = %message_including_full_error_chain,
                    "failed to update account"
                );
            }
            api_err
        }
    }
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
            "invalid account: not found".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests;
