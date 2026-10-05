use super::{ApiError, AppState};
use crate::api::filters::AccountsFilter;
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
use std::collections::HashMap;
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
    // Default view hides retired accounts; `include_inactive=true` opts into
    // the full roster (e.g. an admin screen that needs to see/reinstate a
    // retired account).
    let accounts = if filter.include_inactive.unwrap_or(false) {
        trading_accounts::list_all(&state.pool).await
    } else {
        trading_accounts::list_active(&state.pool).await
    }
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

/// Defense in depth: reject an account creation or reinstatement for an
/// exchange that has no `[exchange_margin.<name>]` entry, or whose entry is
/// non-positive.
///
/// The startup gate (`resolve_exchange_liquidation_levels`) only enforces
/// `Y > 0` for exchanges that already have an active account; entries on
/// unused exchanges are tolerated so a stale TOML cannot brick boot. That
/// tolerance opens a footgun if we don't re-check here: an operator could
/// create — or reinstate — the first active account on an exchange whose Y
/// is 0/negative, the request would succeed, and PositionSizer would then
/// return None on every signal — surfacing as a misleading "balance too
/// small" error. Validate the value strictly positive here to fail loudly,
/// since both creation and reinstatement produce an account that is
/// immediately eligible for new trades.
fn validate_liquidation_level_for_exchange(
    levels: &HashMap<Exchange, Decimal>,
    exchange_enum: Exchange,
    exchange_normalized: &str,
) -> Result<(), ApiError> {
    match levels.get(&exchange_enum) {
        None => Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!(
                "exchange '{}' has no [exchange_margin.{}] entry in config; \
                 add `liquidation_margin_level` and restart the service before \
                 creating accounts on this exchange",
                exchange_normalized, exchange_normalized
            ),
        )),
        Some(y) if *y <= Decimal::ZERO => Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!(
                "exchange '{}' has [exchange_margin.{}].liquidation_margin_level = {}, \
                 which must be > 0; fix the value in config and restart the service \
                 before creating accounts on this exchange",
                exchange_normalized, exchange_normalized, y
            ),
        )),
        Some(_) => Ok(()),
    }
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
    if let Err(msg) = trading_accounts::validate_leverage_for_exchange(exchange_enum, req.leverage)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, msg));
    }
    // Defense in depth: see `validate_liquidation_level_for_exchange` for why
    // this is re-checked here instead of relying solely on the startup gate.
    validate_liquidation_level_for_exchange(
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
            format!(
                "strategy '{}' not found in catalog (see GET /api/strategies)",
                req.strategy
            ),
        ));
    }
    // Duplicate live account per exchange: the DB partial unique index is
    // the real guard, but pre-check here for a friendly 409. `AND active`:
    // a retired (active = FALSE) live account must not block creating a new
    // live account on the same exchange — see the matching pre-check and
    // partial unique index in trading_accounts.rs::create_account.
    if req.account_type == "live" {
        let existing: Option<(uuid::Uuid,)> = sqlx::query_as(
            "SELECT id FROM trading_accounts WHERE exchange = $1 AND account_type = 'live' AND active LIMIT 1",
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

/// Update a trading account's mutable fields (`name`, `leverage`,
/// `strategy`, `active`).
///
/// Reinstating an account (`active: false` → `true`) makes it eligible for
/// new trades starting from the next signal. Per-exchange liquidation levels
/// are re-validated against the process-startup-resolved map at reinstatement
/// time (`validate_liquidation_level_for_exchange`, same guard as `create`),
/// but the `fx_new` leverage/exchange assumption check is not — see
/// `UpdateTradingAccount::active` for the exact conditions under which that
/// one still requires a process restart to take effect.
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
    // Leverage cap pre-check at the HTTP boundary so a violation returns 400
    // with the human-readable cap message. (`update_account` re-checks for
    // non-HTTP callers but its anyhow::bail! would otherwise surface as a
    // generic 500 here.)
    //
    // Uses let-chains (`if let && let`) so the flat form avoids
    // clippy::collapsible_if on nested `if let`.
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
    // Retiring an account would orphan any open or closing position still on
    // it (no task would apply fees / monitor margin / resolve a liquidation
    // level for it once it drops off `list_active`). `update_account`
    // re-checks this too (defense in depth for non-HTTP callers). That check
    // runs inside the same UPDATE statement, so no other query can slip in
    // between check and write — reliable against committed trades, though not
    // serialized against a concurrent transaction's uncommitted trade INSERT
    // (READ COMMITTED). The bare `anyhow::bail!` would surface as a generic
    // 500 here —
    // pre-check at the HTTP boundary so the response is a precise 409 with
    // the open/closing-trade count the operator needs to act on.
    if req.active == Some(false) {
        let unsettled = trades::list_open_or_closing_by_account(&state.pool, id)
            .await
            .map_err(ApiError::from)?;
        if !unsettled.is_empty() {
            return Err(ApiError(
                StatusCode::CONFLICT,
                format!(
                    "cannot deactivate account: {} open/closing trade(s) exist; close them first",
                    unsettled.len()
                ),
            ));
        }
    }
    // Reinstating an account (active: false -> true) makes it eligible for
    // new trades, same as a brand new `create` — so it must pass the same
    // liquidation-level guard (see `validate_liquidation_level_for_exchange`)
    // regardless of account_type. A live account must additionally not
    // collide with another already-active live account on the same exchange
    // (shared singleton API client — same reason `create` rejects a second
    // live row per exchange). Both checks share the single account fetch
    // below to avoid a second round-trip.
    if req.active == Some(true)
        && let Some(account) = trading_accounts::get(&state.pool, id)
            .await
            .map_err(ApiError::from)?
    {
        let exchange_enum: Exchange = account
            .exchange
            .parse()
            .map_err(|e: anyhow::Error| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
        validate_liquidation_level_for_exchange(
            &state.exchange_liquidation_levels,
            exchange_enum,
            &account.exchange,
        )?;

        if account.account_type == "live" {
            let existing: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM trading_accounts
                 WHERE exchange = $1 AND account_type = 'live' AND active AND id <> $2
                 LIMIT 1",
            )
            .bind(&account.exchange)
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::from(anyhow::Error::from(e)))?;
            if let Some((other_id,)) = existing {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    format!(
                        "cannot reactivate account: live account for exchange '{}' already active (id={}); only 1 active live account per exchange is supported",
                        account.exchange, other_id
                    ),
                ));
            }
        }
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
