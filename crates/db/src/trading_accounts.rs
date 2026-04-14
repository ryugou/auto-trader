//! Trading account DB access.
//!
//! Unified account row replacing the old `paper_accounts` table.
//! Backed by the `trading_accounts` table from migration 20260415000001.

use chrono::{DateTime, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Hard minimum initial balance for JPY-denominated accounts.
pub const MIN_INITIAL_BALANCE_JPY: Decimal = dec!(10000);

/// Normalize a currency code: trim ASCII whitespace + uppercase.
pub fn normalize_currency(currency: &str) -> String {
    currency
        .trim_matches([' ', '\t', '\n', '\r'])
        .to_ascii_uppercase()
}

/// Validate that a currency / initial_balance satisfies the minimum-balance rule.
pub fn validate_initial_balance(currency: &str, initial_balance: Decimal) -> Result<(), String> {
    if normalize_currency(currency) == "JPY" && initial_balance < MIN_INITIAL_BALANCE_JPY {
        return Err(format!(
            "initial_balance must be at least {MIN_INITIAL_BALANCE_JPY} JPY"
        ));
    }
    Ok(())
}

/// Unified account row (replaces PaperAccount).
#[derive(Debug, Clone, Serialize)]
pub struct TradingAccount {
    pub id: Uuid,
    pub name: String,
    pub account_type: String,
    pub exchange: String,
    pub strategy: String,
    pub initial_balance: Decimal,
    pub current_balance: Decimal,
    pub leverage: Decimal,
    pub currency: String,
    pub created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: Uuid,
    name: String,
    account_type: String,
    exchange: String,
    strategy: String,
    initial_balance: Decimal,
    current_balance: Decimal,
    leverage: Decimal,
    currency: String,
    created_at: DateTime<Utc>,
}

impl From<AccountRow> for TradingAccount {
    fn from(r: AccountRow) -> Self {
        TradingAccount {
            id: r.id,
            name: r.name,
            account_type: r.account_type,
            exchange: r.exchange,
            strategy: r.strategy,
            initial_balance: r.initial_balance,
            current_balance: r.current_balance,
            leverage: r.leverage,
            currency: r.currency,
            created_at: r.created_at,
        }
    }
}

const ACCOUNT_COLUMNS: &str = "id, name, account_type, exchange, strategy, \
                                initial_balance, current_balance, leverage, currency, created_at";

/// Fetch a single account by id.
pub async fn get_account(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<TradingAccount>> {
    let row = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(TradingAccount::from))
}

/// List all accounts ordered by created_at.
pub async fn list_all(pool: &PgPool) -> anyhow::Result<Vec<TradingAccount>> {
    let rows = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts ORDER BY created_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(TradingAccount::from).collect())
}

/// Update current_balance for an account.
pub async fn update_balance(pool: &PgPool, id: Uuid, new_balance: Decimal) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE trading_accounts SET current_balance = $2 WHERE id = $1")
        .bind(id)
        .bind(new_balance)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("trading account {id} not found when updating balance");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CRUD types and functions (for REST API)
// ---------------------------------------------------------------------------

fn default_currency() -> String {
    "JPY".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CreateTradingAccount {
    pub name: String,
    pub exchange: String,
    pub initial_balance: Decimal,
    pub leverage: Decimal,
    pub strategy: String,
    pub account_type: String,
    #[serde(default = "default_currency")]
    pub currency: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTradingAccount {
    pub name: Option<String>,
    pub leverage: Option<Decimal>,
    pub strategy: Option<String>,
}

pub async fn create_account(
    pool: &PgPool,
    req: &CreateTradingAccount,
) -> anyhow::Result<TradingAccount> {
    // Defense in depth: `create_account` is callable from non-HTTP paths
    // (CLI, tests, future internal callers), and the HTTP deserializer does
    // not constrain this field. Reject invalid values here so a bad string
    // never reaches the DB CHECK constraint.
    if req.account_type != "paper" && req.account_type != "live" {
        anyhow::bail!(
            "invalid account_type '{}' (must be 'paper' or 'live')",
            req.account_type
        );
    }
    let currency = normalize_currency(&req.currency);
    if let Err(msg) = validate_initial_balance(&currency, req.initial_balance) {
        anyhow::bail!(msg);
    }
    let initial_balance = if currency == "JPY" {
        req.initial_balance
            .round_dp_with_strategy(0, RoundingStrategy::ToZero)
    } else {
        req.initial_balance
    };
    let id = Uuid::new_v4();
    let sql = format!(
        r#"INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                                          initial_balance, current_balance, leverage, currency)
           VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8)
           RETURNING {ACCOUNT_COLUMNS}"#
    );
    let row = sqlx::query_as::<_, AccountRow>(&sql)
        .bind(id)
        .bind(&req.name)
        .bind(&req.account_type)
        .bind(&req.exchange)
        .bind(&req.strategy)
        .bind(initial_balance)
        .bind(req.leverage)
        .bind(&currency)
        .fetch_one(pool)
        .await?;
    Ok(TradingAccount::from(row))
}

pub async fn update_account(
    pool: &PgPool,
    id: Uuid,
    req: &UpdateTradingAccount,
) -> anyhow::Result<Option<TradingAccount>> {
    let sql = format!(
        r#"UPDATE trading_accounts SET
               name = COALESCE($2, name),
               leverage = COALESCE($3, leverage),
               strategy = COALESCE($4, strategy)
           WHERE id = $1
           RETURNING {ACCOUNT_COLUMNS}"#
    );
    let row = sqlx::query_as::<_, AccountRow>(&sql)
        .bind(id)
        .bind(&req.name)
        .bind(req.leverage)
        .bind(&req.strategy)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(TradingAccount::from))
}

pub async fn delete_account(pool: &PgPool, id: Uuid) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM trading_accounts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
