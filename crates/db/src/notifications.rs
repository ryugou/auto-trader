//! Notification DB access.

use auto_trader_core::types::Trade;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    pub trade_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
    pub strategy_name: Option<String>,
    pub pair: Option<String>,
    pub direction: Option<String>,
    pub price: Option<Decimal>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

/// Insert a `trade_opened` notification.
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

/// Insert a `trade_closed` notification.
pub async fn insert_trade_closed<'e, E>(executor: E, trade: &Trade) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let direction = trade.direction.as_str();
    let exit_reason_str = trade.exit_reason.map(|r| r.as_str());
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
    .bind(trade.exit_price)
    .bind(trade.pnl_amount)
    .bind(exit_reason_str)
    .execute(executor)
    .await?;
    Ok(())
}

/// Paginated list with optional filters.
pub async fn list(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    unread_only: bool,
    kind_filter: Option<&str>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<(Vec<Notification>, i64)> {
    let jst_offset =
        chrono::FixedOffset::east_opt(9 * 3600).expect("9-hour offset is always valid");
    let from_ts = from.map(|d| {
        d.and_hms_opt(0, 0, 0)
            .expect("midnight is always valid in fixed-offset JST")
            .and_local_timezone(jst_offset)
            .single()
            .expect("midnight in JST fixed-offset is always valid")
            .with_timezone(&Utc)
    });
    let to_ts = to.map(|d| {
        (d + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .expect("midnight is always valid in fixed-offset JST")
            .and_local_timezone(jst_offset)
            .single()
            .expect("midnight in JST fixed-offset is always valid")
            .with_timezone(&Utc)
    });

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

pub async fn unread_count(pool: &PgPool) -> anyhow::Result<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE read_at IS NULL")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

pub async fn mark_all_read(pool: &PgPool) -> anyhow::Result<u64> {
    let result = sqlx::query("UPDATE notifications SET read_at = NOW() WHERE read_at IS NULL")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

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
mod tests {
    use auto_trader_core::types::ExitReason;

    #[test]
    fn exit_reason_as_str_no_quotes() {
        let s = ExitReason::SlHit.as_str();
        assert!(!s.starts_with('"'));
        assert!(!s.ends_with('"'));
        assert!(!s.is_empty());
    }
}
