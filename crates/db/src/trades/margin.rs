use rust_decimal::Decimal;
use uuid::Uuid;

pub async fn lock_margin(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    margin_amount: Decimal,
) -> anyhow::Result<()> {
    let new_balance: Decimal = sqlx::query_scalar(
        r#"UPDATE trading_accounts
           SET current_balance = current_balance - $2
           WHERE id = $1
           RETURNING current_balance"#,
    )
    .bind(account_id)
    .bind(margin_amount)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
           VALUES ($1, $2, 'margin_lock', $3, $4)"#,
    )
    .bind(account_id)
    .bind(trade_id)
    .bind(-margin_amount)
    .bind(new_balance)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

pub async fn release_margin(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    margin_return: Decimal,
    pnl_amount: Decimal,
) -> anyhow::Result<()> {
    let new_balance: Decimal = sqlx::query_scalar(
        r#"UPDATE trading_accounts
           SET current_balance = current_balance + $2 + $3
           WHERE id = $1
           RETURNING current_balance"#,
    )
    .bind(account_id)
    .bind(margin_return)
    .bind(pnl_amount)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
           VALUES ($1, $2, 'margin_release', $3, $4)"#,
    )
    .bind(account_id)
    .bind(trade_id)
    .bind(margin_return)
    .bind(new_balance - pnl_amount)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
           VALUES ($1, $2, 'trade_close', $3, $4)"#,
    )
    .bind(account_id)
    .bind(trade_id)
    .bind(pnl_amount)
    .bind(new_balance)
    .execute(&mut *tx)
    .await?;

    Ok(())
}
