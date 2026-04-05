use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SummaryStats {
    pub total_trades: i64,
    pub win_count: i64,
    pub total_pnl: Decimal,
    pub max_drawdown: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PnlHistoryRow {
    pub date: NaiveDate,
    pub daily_pnl: Decimal,
    pub cumulative_pnl: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct StrategyStats {
    pub strategy_name: String,
    pub trade_count: i64,
    pub win_count: i64,
    pub total_pnl: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PairStats {
    pub pair: String,
    pub trade_count: i64,
    pub win_count: i64,
    pub total_pnl: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct HourlyWinrate {
    pub hour: i32,
    pub trade_count: i64,
    pub win_count: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TradeRow {
    pub id: Uuid,
    pub strategy_name: String,
    pub pair: String,
    pub exchange: String,
    pub direction: String,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub quantity: Option<Decimal>,
    pub leverage: Decimal,
    pub fees: Decimal,
    pub paper_account_id: Option<Uuid>,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_pips: Option<Decimal>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<String>,
    pub mode: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// Dashboard summary: total trades, win count, total PnL, max drawdown.
pub async fn get_summary(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<SummaryStats> {
    let row = sqlx::query_as::<_, SummaryStats>(
        r#"SELECT
               COALESCE(SUM(trade_count), 0)::bigint   AS total_trades,
               COALESCE(SUM(win_count), 0)::bigint     AS win_count,
               COALESCE(SUM(total_pnl), 0)             AS total_pnl,
               COALESCE(MAX(max_drawdown), 0)           AS max_drawdown
           FROM daily_summary
           WHERE ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR paper_account_id = $2)
             AND ($3::date IS NULL OR date >= $3)
             AND ($4::date IS NULL OR date <= $4)"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Daily PnL history with cumulative sum (window function).
pub async fn get_pnl_history(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<Vec<PnlHistoryRow>> {
    let rows = sqlx::query_as::<_, PnlHistoryRow>(
        r#"SELECT
               date,
               SUM(total_pnl) AS daily_pnl,
               SUM(SUM(total_pnl)) OVER (ORDER BY date) AS cumulative_pnl
           FROM daily_summary
           WHERE ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR paper_account_id = $2)
             AND ($3::date IS NULL OR date >= $3)
             AND ($4::date IS NULL OR date <= $4)
           GROUP BY date
           ORDER BY date"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Per-strategy aggregates.
pub async fn get_strategy_stats(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
) -> anyhow::Result<Vec<StrategyStats>> {
    let rows = sqlx::query_as::<_, StrategyStats>(
        r#"SELECT
               strategy_name,
               COALESCE(SUM(trade_count), 0)::bigint AS trade_count,
               COALESCE(SUM(win_count), 0)::bigint   AS win_count,
               COALESCE(SUM(total_pnl), 0)           AS total_pnl
           FROM daily_summary
           WHERE ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR paper_account_id = $2)
           GROUP BY strategy_name
           ORDER BY total_pnl DESC"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Per-pair aggregates.
pub async fn get_pair_stats(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
) -> anyhow::Result<Vec<PairStats>> {
    let rows = sqlx::query_as::<_, PairStats>(
        r#"SELECT
               pair,
               COALESCE(SUM(trade_count), 0)::bigint AS trade_count,
               COALESCE(SUM(win_count), 0)::bigint   AS win_count,
               COALESCE(SUM(total_pnl), 0)           AS total_pnl
           FROM daily_summary
           WHERE ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR paper_account_id = $2)
           GROUP BY pair
           ORDER BY total_pnl DESC"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Win rate grouped by hour of day (based on entry_at).
pub async fn get_hourly_winrate(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
) -> anyhow::Result<Vec<HourlyWinrate>> {
    let rows = sqlx::query_as::<_, HourlyWinrate>(
        r#"SELECT
               EXTRACT(HOUR FROM entry_at)::int AS hour,
               COUNT(*)::bigint                 AS trade_count,
               COUNT(*) FILTER (WHERE pnl_amount > 0)::bigint AS win_count
           FROM trades
           WHERE status = 'closed'
             AND ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR paper_account_id = $2)
           GROUP BY hour
           ORDER BY hour"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Paginated trade list with optional filters. Returns (rows, total_count).
#[allow(clippy::too_many_arguments)]
pub async fn get_trades(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
    strategy: Option<&str>,
    pair: Option<&str>,
    status: Option<&str>,
    page: Option<i64>,
    per_page: Option<i64>,
) -> anyhow::Result<(Vec<TradeRow>, i64)> {
    let limit = per_page.unwrap_or(50).min(200);
    let offset = (page.unwrap_or(1).max(1) - 1) * limit;

    let total: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)::bigint
           FROM trades
           WHERE ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR paper_account_id = $2)
             AND ($3::text IS NULL OR strategy_name = $3)
             AND ($4::text IS NULL OR pair = $4)
             AND ($5::text IS NULL OR status = $5)"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(strategy)
    .bind(pair)
    .bind(status)
    .fetch_one(pool)
    .await?;

    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction,
                  entry_price, exit_price, stop_loss, take_profit,
                  quantity, leverage, fees, paper_account_id,
                  entry_at, exit_at, pnl_pips, pnl_amount,
                  exit_reason, mode, status, created_at
           FROM trades
           WHERE ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR paper_account_id = $2)
             AND ($3::text IS NULL OR strategy_name = $3)
             AND ($4::text IS NULL OR pair = $4)
             AND ($5::text IS NULL OR status = $5)
           ORDER BY created_at DESC
           LIMIT $6 OFFSET $7"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(strategy)
    .bind(pair)
    .bind(status)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    Ok((rows, total.0))
}
