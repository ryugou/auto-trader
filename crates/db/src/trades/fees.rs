use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

async fn apply_fee_inner(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
    event_type: &'static str,
    occurred_at: Option<DateTime<Utc>>,
) -> anyhow::Result<Option<Decimal>> {
    let trade_updated = sqlx::query(
        "UPDATE trades SET fees = fees + $3
         WHERE id = $1 AND account_id = $2 AND status = 'open'",
    )
    .bind(trade_id)
    .bind(account_id)
    .bind(fee_amount)
    .execute(&mut *tx)
    .await?;

    if trade_updated.rows_affected() == 0 {
        return Ok(None);
    }

    let new_balance: Decimal = sqlx::query_scalar(
        r#"UPDATE trading_accounts
           SET current_balance = current_balance - $2
           WHERE id = $1
           RETURNING current_balance"#,
    )
    .bind(account_id)
    .bind(fee_amount)
    .fetch_one(&mut *tx)
    .await?;

    let sql = match occurred_at {
        Some(_) => {
            r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after, occurred_at)
                   VALUES ($1, $2, $3, $4, $5, $6)"#
        }
        None => {
            r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
                   VALUES ($1, $2, $3, $4, $5)"#
        }
    };
    let mut insert = sqlx::query(sql)
        .bind(account_id)
        .bind(trade_id)
        .bind(event_type)
        .bind(-fee_amount)
        .bind(new_balance);
    if let Some(at) = occurred_at {
        insert = insert.bind(at);
    }
    insert.execute(&mut *tx).await?;

    Ok(Some(new_balance))
}

pub async fn apply_overnight_fee(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
) -> anyhow::Result<Option<Decimal>> {
    apply_fee_inner(tx, account_id, trade_id, fee_amount, "overnight_fee", None).await
}

pub async fn apply_sfd_fee(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
    occurred_at: DateTime<Utc>,
) -> anyhow::Result<Option<Decimal>> {
    apply_fee_inner(
        tx,
        account_id,
        trade_id,
        fee_amount,
        "sfd_fee",
        Some(occurred_at),
    )
    .await
}

pub async fn apply_swap_fee(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
    occurred_at: DateTime<Utc>,
) -> anyhow::Result<Option<Decimal>> {
    apply_fee_inner(
        tx,
        account_id,
        trade_id,
        fee_amount,
        "swap_fee",
        Some(occurred_at),
    )
    .await
}

pub async fn add_fees(pool: &PgPool, id: Uuid, fee_delta: Decimal) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE trades SET fees = fees + $2 WHERE id = $1")
        .bind(id)
        .bind(fee_delta)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("failed to add fees for trade: not found");
    }
    Ok(())
}
