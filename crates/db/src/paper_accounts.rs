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
    #[serde(default = "default_currency")]
    pub currency: String,
}

fn default_currency() -> String {
    "JPY".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdatePaperAccount {
    pub name: Option<String>,
    pub initial_balance: Option<Decimal>,
    pub leverage: Option<Decimal>,
    pub strategy: Option<String>,
}

pub async fn list_paper_accounts(pool: &PgPool) -> anyhow::Result<Vec<PaperAccount>> {
    let accounts = sqlx::query_as::<_, PaperAccount>(
        "SELECT id, name, exchange, initial_balance, current_balance, currency, leverage, strategy, created_at, updated_at
         FROM paper_accounts ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(accounts)
}

pub async fn get_paper_account(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<PaperAccount>> {
    let account = sqlx::query_as::<_, PaperAccount>(
        "SELECT id, name, exchange, initial_balance, current_balance, currency, leverage, strategy, created_at, updated_at
         FROM paper_accounts WHERE id = $1",
    )
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
    let account = sqlx::query_as::<_, PaperAccount>(
        r#"INSERT INTO paper_accounts (id, name, exchange, initial_balance, current_balance, currency, leverage, strategy)
           VALUES ($1, $2, $3, $4, $4, $5, $6, $7)
           RETURNING id, name, exchange, initial_balance, current_balance, currency, leverage, strategy, created_at, updated_at"#,
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.exchange)
    .bind(req.initial_balance)
    .bind(&req.currency)
    .bind(req.leverage)
    .bind(&req.strategy)
    .fetch_one(pool)
    .await?;
    Ok(account)
}

pub async fn update_paper_account(
    pool: &PgPool,
    id: Uuid,
    req: &UpdatePaperAccount,
) -> anyhow::Result<Option<PaperAccount>> {
    let account = sqlx::query_as::<_, PaperAccount>(
        r#"UPDATE paper_accounts SET
            name = COALESCE($2, name),
            initial_balance = COALESCE($3, initial_balance),
            leverage = COALESCE($4, leverage),
            strategy = COALESCE($5, strategy),
            updated_at = NOW()
           WHERE id = $1
           RETURNING id, name, exchange, initial_balance, current_balance, currency, leverage, strategy, created_at, updated_at"#,
    )
    .bind(id)
    .bind(&req.name)
    .bind(req.initial_balance)
    .bind(req.leverage)
    .bind(&req.strategy)
    .fetch_optional(pool)
    .await?;
    Ok(account)
}

/// Add a P&L delta to current_balance (positive or negative).
pub async fn add_pnl(pool: &PgPool, id: Uuid, pnl_delta: Decimal) -> anyhow::Result<()> {
    sqlx::query("UPDATE paper_accounts SET current_balance = current_balance + $2, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .bind(pnl_delta)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_paper_account(pool: &PgPool, id: Uuid) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM paper_accounts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

