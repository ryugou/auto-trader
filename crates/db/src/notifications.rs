use auto_trader_core::types::{Direction, ExitReason, Trade};
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

fn direction_str(d: Direction) -> &'static str {
    match d {
        Direction::Long => "long",
        Direction::Short => "short",
    }
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
        .ok_or_else(|| anyhow::anyhow!("trade has no paper_account_id"))?;
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
    .bind(direction_str(trade.direction))
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
        .ok_or_else(|| anyhow::anyhow!("trade has no paper_account_id"))?;
    let price = trade
        .exit_price
        .ok_or_else(|| anyhow::anyhow!("closed trade has no exit_price"))?;
    let pnl = trade
        .pnl_amount
        .ok_or_else(|| anyhow::anyhow!("closed trade has no pnl_amount"))?;
    let reason = trade
        .exit_reason
        .ok_or_else(|| anyhow::anyhow!("closed trade has no exit_reason"))?;
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
    .bind(direction_str(trade.direction))
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
    let jst_offset = chrono::FixedOffset::east_opt(9 * 3600).expect("fixed offset");
    let from_ts = from.map(|d| {
        d.and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(jst_offset)
            .unwrap()
            .with_timezone(&Utc)
    });
    let to_ts = to.map(|d| {
        (d + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(jst_offset)
            .unwrap()
            .with_timezone(&Utc)
    });

    let mut sql = String::from(
        "SELECT id, kind, trade_id, paper_account_id, strategy_name, pair,
                direction, price, pnl_amount, exit_reason, created_at, read_at
         FROM notifications WHERE 1=1",
    );
    if unread_only {
        sql.push_str(" AND read_at IS NULL");
    }
    if kind_filter.is_some() {
        sql.push_str(" AND kind = $1");
    }
    let mut placeholder = if kind_filter.is_some() { 2 } else { 1 };
    if from_ts.is_some() {
        sql.push_str(&format!(" AND created_at >= ${placeholder}"));
        placeholder += 1;
    }
    if to_ts.is_some() {
        sql.push_str(&format!(" AND created_at < ${placeholder}"));
        placeholder += 1;
    }
    sql.push_str(&format!(
        " ORDER BY created_at DESC LIMIT ${} OFFSET ${}",
        placeholder,
        placeholder + 1
    ));

    let mut q = sqlx::query_as::<_, Notification>(&sql);
    if let Some(k) = kind_filter {
        q = q.bind(k);
    }
    if let Some(f) = from_ts {
        q = q.bind(f);
    }
    if let Some(t) = to_ts {
        q = q.bind(t);
    }
    q = q.bind(limit).bind(offset);
    let items = q.fetch_all(pool).await?;

    let mut count_sql = String::from("SELECT COUNT(*) FROM notifications WHERE 1=1");
    if unread_only {
        count_sql.push_str(" AND read_at IS NULL");
    }
    if kind_filter.is_some() {
        count_sql.push_str(" AND kind = $1");
    }
    let mut count_ph = if kind_filter.is_some() { 2 } else { 1 };
    if from_ts.is_some() {
        count_sql.push_str(&format!(" AND created_at >= ${count_ph}"));
        count_ph += 1;
    }
    if to_ts.is_some() {
        count_sql.push_str(&format!(" AND created_at < ${count_ph}"));
    }
    let mut cq = sqlx::query_scalar::<_, i64>(&count_sql);
    if let Some(k) = kind_filter {
        cq = cq.bind(k);
    }
    if let Some(f) = from_ts {
        cq = cq.bind(f);
    }
    if let Some(t) = to_ts {
        cq = cq.bind(t);
    }
    let total: i64 = cq.fetch_one(pool).await?;

    Ok((items, total))
}

pub async fn unread_count(pool: &PgPool) -> anyhow::Result<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE read_at IS NULL")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

pub async fn mark_all_read(pool: &PgPool) -> anyhow::Result<i64> {
    let result = sqlx::query("UPDATE notifications SET read_at = NOW() WHERE read_at IS NULL")
        .execute(pool)
        .await?;
    Ok(result.rows_affected() as i64)
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
    use auto_trader_core::types::Direction;

    #[test]
    fn direction_str_maps_long_short() {
        assert_eq!(direction_str(Direction::Long), "long");
        assert_eq!(direction_str(Direction::Short), "short");
    }

    #[test]
    fn exit_reason_str_strips_quotes() {
        let s = exit_reason_str(ExitReason::SlHit);
        assert!(!s.starts_with('"'));
        assert!(!s.ends_with('"'));
        assert!(!s.is_empty());
    }
}
