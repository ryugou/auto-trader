use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

/// Upsert a paper account by name. Returns the account UUID.
/// If the account already exists (by name), updates balance/leverage to match config
/// and returns the existing ID.
pub async fn upsert_paper_account(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    exchange: &str,
    initial_balance: Decimal,
    leverage: Decimal,
    currency: &str,
) -> anyhow::Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        r#"INSERT INTO paper_accounts (id, name, exchange, initial_balance, current_balance, currency, leverage)
           VALUES ($1, $2, $3, $4, $4, $5, $6)
           ON CONFLICT (name) DO UPDATE
           SET initial_balance = EXCLUDED.initial_balance,
               leverage = EXCLUDED.leverage,
               updated_at = NOW()
           RETURNING id"#,
    )
    .bind(id)
    .bind(name)
    .bind(exchange)
    .bind(initial_balance)
    .bind(currency)
    .bind(leverage)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}
