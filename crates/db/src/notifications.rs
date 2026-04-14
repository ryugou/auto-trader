use auto_trader_core::types::{ExitReason, Trade};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    pub trade_id: Uuid,
    pub paper_account_id: Uuid,
    pub strategy_name: String,
    pub pair: String,
    pub direction: String,
    pub price: Decimal,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

fn exit_reason_str(r: ExitReason) -> String {
    serde_json::to_string(&r)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Insert a `trade_opened` notification. Must be called with the same
/// executor (usually a `&mut tx`) that wrote the `trades` row so that
/// the two live or die together.
pub async fn insert_trade_opened<'e, E>(executor: E, trade: &Trade) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let account_id = trade
        .paper_account_id
        .ok_or_else(|| anyhow::anyhow!("trade {} has no paper_account_id", trade.id))?;
    sqlx::query(
        r#"INSERT INTO notifications
               (kind, trade_id, paper_account_id, strategy_name, pair,
                direction, price)
           VALUES ('trade_opened', $1, $2, $3, $4, $5, $6)"#,
    )
    .bind(trade.id)
    .bind(account_id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(trade.direction.as_str())
    .bind(trade.entry_price)
    .execute(executor)
    .await?;
    Ok(())
}

/// Insert a `trade_closed` notification. `trade` must have `exit_price`,
/// `pnl_amount`, and `exit_reason` populated.
pub async fn insert_trade_closed<'e, E>(executor: E, trade: &Trade) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let account_id = trade
        .paper_account_id
        .ok_or_else(|| anyhow::anyhow!("trade {} has no paper_account_id", trade.id))?;
    let price = trade
        .exit_price
        .ok_or_else(|| anyhow::anyhow!("closed trade {} has no exit_price", trade.id))?;
    let pnl = trade
        .pnl_amount
        .ok_or_else(|| anyhow::anyhow!("closed trade {} has no pnl_amount", trade.id))?;
    let reason = trade
        .exit_reason
        .ok_or_else(|| anyhow::anyhow!("closed trade {} has no exit_reason", trade.id))?;
    sqlx::query(
        r#"INSERT INTO notifications
               (kind, trade_id, paper_account_id, strategy_name, pair,
                direction, price, pnl_amount, exit_reason)
           VALUES ('trade_closed', $1, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(trade.id)
    .bind(account_id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(trade.direction.as_str())
    .bind(price)
    .bind(pnl)
    .bind(exit_reason_str(reason))
    .execute(executor)
    .await?;
    Ok(())
}

/// Paginated list with optional filters. Dates are interpreted as JST
/// (UTC+9) day boundaries to match the rest of the dashboard — a
/// `from = 2026-04-08` means "trades created from 2026-04-08 00:00 JST".
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
    // Convert JST day boundaries to half-open [from, to+1day) UTC
    // timestamps so the existing `created_at` index can be used and
    // the bounds check is unambiguous about including `to`.
    let from_ts = from.map(|d| {
        d.and_hms_opt(0, 0, 0)
            .expect("midnight is always a valid time")
            .and_local_timezone(jst_offset)
            .single()
            .expect("midnight in a fixed-offset zone has no DST ambiguity")
            .with_timezone(&Utc)
    });
    let to_ts = to.map(|d| {
        (d + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .expect("midnight is always a valid time")
            .and_local_timezone(jst_offset)
            .single()
            .expect("midnight in a fixed-offset zone has no DST ambiguity")
            .with_timezone(&Utc)
    });

    // Single helper that appends WHERE clauses to either the SELECT or
    // the COUNT(*) builder so they cannot drift apart. The builder
    // handles placeholder numbering for us — no manual `$N` math, and
    // adding a future filter is mechanical.
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
        "SELECT id, kind, trade_id, paper_account_id, strategy_name, pair, \
         direction, price, pnl_amount, exit_reason, created_at, read_at \
         FROM notifications WHERE 1=1",
    );
    apply_filters(&mut select_qb, unread_only, kind_filter, from_ts, to_ts);
    // ORDER BY has a stable tie-breaker on `id` so pagination doesn't
    // drop or duplicate rows when multiple notifications share the
    // same `created_at` timestamp (rare in practice, but two
    // simultaneous open/close events can land in the same tick).
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

/// Delete read notifications older than 30 days. Returns rows deleted.
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
    use super::*;

    #[test]
    fn exit_reason_str_strips_quotes() {
        let s = exit_reason_str(ExitReason::SlHit);
        assert!(!s.starts_with('"'));
        assert!(!s.ends_with('"'));
        assert!(!s.is_empty());
    }
}
