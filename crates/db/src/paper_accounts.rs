use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PaperAccount {
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
}

#[derive(Debug, Deserialize)]
pub struct CreatePaperAccount {
    pub name: String,
    pub exchange: String,
    pub initial_balance: Decimal,
    pub leverage: Decimal,
    pub strategy: String,
    pub account_type: String,
    #[serde(default = "default_currency")]
    pub currency: String,
}

fn default_currency() -> String {
    "JPY".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdatePaperAccount {
    pub name: Option<String>,
    pub leverage: Option<Decimal>,
    pub strategy: Option<String>,
}

const ACCOUNT_COLUMNS: &str = "id, name, exchange, initial_balance, current_balance, currency, leverage, strategy, account_type, created_at, updated_at";

pub async fn list_paper_accounts(pool: &PgPool) -> anyhow::Result<Vec<PaperAccount>> {
    let sql = format!(
        "SELECT {ACCOUNT_COLUMNS} FROM paper_accounts ORDER BY created_at ASC"
    );
    let accounts = sqlx::query_as::<_, PaperAccount>(&sql)
        .fetch_all(pool)
        .await?;
    Ok(accounts)
}

pub async fn get_paper_account(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<PaperAccount>> {
    let sql = format!(
        "SELECT {ACCOUNT_COLUMNS} FROM paper_accounts WHERE id = $1"
    );
    let account = sqlx::query_as::<_, PaperAccount>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(account)
}

pub async fn create_paper_account(
    pool: &PgPool,
    req: &CreatePaperAccount,
) -> anyhow::Result<PaperAccount> {
    let id = Uuid::new_v4();
    let sql = format!(
        r#"INSERT INTO paper_accounts (id, name, exchange, initial_balance, current_balance, currency, leverage, strategy, account_type)
           VALUES ($1, $2, $3, $4, $4, $5, $6, $7, $8)
           RETURNING {ACCOUNT_COLUMNS}"#
    );
    let account = sqlx::query_as::<_, PaperAccount>(&sql)
        .bind(id)
        .bind(&req.name)
        .bind(&req.exchange)
        .bind(req.initial_balance)
        .bind(&req.currency)
        .bind(req.leverage)
        .bind(&req.strategy)
        .bind(&req.account_type)
        .fetch_one(pool)
        .await?;
    Ok(account)
}

pub async fn update_paper_account(
    pool: &PgPool,
    id: Uuid,
    req: &UpdatePaperAccount,
) -> anyhow::Result<Option<PaperAccount>> {
    let sql = format!(
        r#"UPDATE paper_accounts SET
            name = COALESCE($2, name),
            leverage = COALESCE($3, leverage),
            strategy = COALESCE($4, strategy),
            updated_at = NOW()
           WHERE id = $1
           RETURNING {ACCOUNT_COLUMNS}"#
    );
    let account = sqlx::query_as::<_, PaperAccount>(&sql)
        .bind(id)
        .bind(&req.name)
        .bind(req.leverage)
        .bind(&req.strategy)
        .fetch_optional(pool)
        .await?;
    Ok(account)
}

/// Add a P&L delta to current_balance (positive or negative).
/// Returns error if the account does not exist.
pub async fn add_pnl(pool: &PgPool, id: Uuid, pnl_delta: Decimal) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE paper_accounts SET current_balance = current_balance + $2, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .bind(pnl_delta)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("paper account {id} not found when updating balance");
    }
    Ok(())
}

pub async fn delete_paper_account(pool: &PgPool, id: Uuid) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM paper_accounts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Look up account_type by id. Returns None when the account does not exist.
pub async fn get_account_type(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<String>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT account_type FROM paper_accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(v,)| v))
}
