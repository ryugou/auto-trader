use auto_trader_core::types::Trade;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::jst_day_start::{jst_day_start_utc, jst_next_day_start_utc};

/// Row of the `notifications` table: a notification about an opened or closed trade.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Notification {
    /// Identifier of the notification.
    pub id: Uuid,
    /// Kind of the notification: `trade_opened` or `trade_closed`.
    pub kind: String,
    /// Identifier of the trade.
    pub trade_id: Uuid,
    /// Identifier of the account that holds the trade.
    pub account_id: Uuid,
    /// Name of the strategy of the trade.
    pub strategy_name: String,
    /// Currency pair of the trade.
    pub pair: String,
    /// Direction of the trade as text.
    pub direction: String,
    /// Entry price for `trade_opened` and exit price for `trade_closed`.
    pub price: Decimal,
    /// Profit or loss of the trade; `None` for `trade_opened`.
    pub pnl_amount: Option<Decimal>,
    /// Reason the trade was closed; `None` for `trade_opened`.
    pub exit_reason: Option<String>,
    /// Time the notification was created.
    pub created_at: DateTime<Utc>,
    /// Time the notification was marked as read; `None` while it is unread.
    pub read_at: Option<DateTime<Utc>>,
}

/// Inserts a `trade_opened` notification for `trade` with its entry price.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn insert_trade_opened<'e, E>(executor: E, trade: &Trade) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let direction = trade.direction.as_str();
    sqlx::query(
        r#"INSERT INTO notifications
               (kind, trade_id, account_id, strategy_name, pair, direction, price,
                pnl_amount, exit_reason)
           VALUES ('trade_opened', $1, $2, $3, $4, $5, $6, NULL, NULL)"#,
    )
    .bind(trade.id)
    .bind(trade.account_id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(direction)
    .bind(trade.entry_price)
    .execute(executor)
    .await?;
    Ok(())
}

/// Inserts a `trade_closed` notification for `trade` with its exit price, profit or loss and exit reason.
///
/// # Errors
///
/// - Returns an error if `trade` has no exit price.
/// - Returns an error if `trade` has no profit or loss.
/// - Returns an error if `trade` has no exit reason.
/// - Returns an error if the database query fails.
pub async fn insert_trade_closed<'e, E>(executor: E, trade: &Trade) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let exit_price = trade
        .exit_price
        .ok_or_else(|| anyhow::anyhow!("invalid trade: missing exit_price"))?;
    let pnl_amount = trade
        .pnl_amount
        .ok_or_else(|| anyhow::anyhow!("invalid trade: missing pnl_amount"))?;
    let exit_reason = trade
        .exit_reason
        .ok_or_else(|| anyhow::anyhow!("invalid trade: missing exit_reason"))?;
    let direction = trade.direction.as_str();
    sqlx::query(
        r#"INSERT INTO notifications
               (kind, trade_id, account_id, strategy_name, pair, direction, price,
                pnl_amount, exit_reason)
           VALUES ('trade_closed', $1, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(trade.id)
    .bind(trade.account_id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(direction)
    .bind(exit_price)
    .bind(pnl_amount)
    .bind(exit_reason.as_str())
    .execute(executor)
    .await?;
    Ok(())
}

/// Returns a page of notifications, newest first, together with the number of notifications that match the filters. `unread_only` keeps only unread notifications and `kind_filter` keeps one kind. `from` and `to` keep the notifications created from the start of the JST day of `from` up to the end of the JST day of `to`.
///
/// # Errors
///
/// - Returns an error if the JST day of `from` or `to` is out of range.
/// - Returns an error if the database query fails.
pub async fn list(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    unread_only: bool,
    kind_filter: Option<&str>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<(Vec<Notification>, i64)> {
    let from_ts = from.map(jst_day_start_utc).transpose()?;
    let to_ts = to.map(jst_next_day_start_utc).transpose()?;

    fn apply_filters<'a>(
        qb: &mut sqlx::QueryBuilder<'a, sqlx::Postgres>,
        unread_only: bool,
        kind_filter: Option<&'a str>,
        from_ts: Option<DateTime<Utc>>,
        to_ts: Option<DateTime<Utc>>,
    ) {
        if unread_only {
            qb.push(" AND read_at IS NULL");
        }
        if let Some(k) = kind_filter {
            qb.push(" AND kind = ").push_bind(k);
        }
        if let Some(f) = from_ts {
            qb.push(" AND created_at >= ").push_bind(f);
        }
        if let Some(t) = to_ts {
            qb.push(" AND created_at < ").push_bind(t);
        }
    }

    let mut select_qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
        "SELECT id, kind, trade_id, account_id, strategy_name, pair, \
         direction, price, pnl_amount, exit_reason, created_at, read_at \
         FROM notifications WHERE 1=1",
    );
    apply_filters(&mut select_qb, unread_only, kind_filter, from_ts, to_ts);
    select_qb
        .push(" ORDER BY created_at DESC, id DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    let items: Vec<Notification> = select_qb
        .build_query_as::<Notification>()
        .fetch_all(pool)
        .await?;

    let mut count_qb: sqlx::QueryBuilder<sqlx::Postgres> =
        sqlx::QueryBuilder::new("SELECT COUNT(*) FROM notifications WHERE 1=1");
    apply_filters(&mut count_qb, unread_only, kind_filter, from_ts, to_ts);
    let total: i64 = count_qb.build_query_scalar::<i64>().fetch_one(pool).await?;

    Ok((items, total))
}

/// Returns the number of notifications that have not been read.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn unread_count(pool: &PgPool) -> anyhow::Result<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE read_at IS NULL")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Marks every unread notification as read at the current time and returns the number of notifications changed.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn mark_all_read(pool: &PgPool) -> anyhow::Result<u64> {
    let result = sqlx::query("UPDATE notifications SET read_at = NOW() WHERE read_at IS NULL")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Deletes the notifications that were read more than 30 days ago and returns the number deleted.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn purge_old_read(pool: &PgPool) -> anyhow::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM notifications
         WHERE read_at IS NOT NULL
           AND read_at < NOW() - INTERVAL '30 days'",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests;
