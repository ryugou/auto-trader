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
    pub total_fees: Decimal,
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
    pub account_type: Option<String>,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_pips: Option<Decimal>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<String>,
    pub mode: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EvaluatedBalance {
    pub current_balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub evaluated_balance: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct BalanceHistoryPoint {
    pub date: NaiveDate,
    pub balance: Decimal,
}

#[derive(Debug, Serialize)]
pub struct BalanceHistoryAccount {
    pub account_id: Uuid,
    pub account_name: String,
    pub data: Vec<BalanceHistoryPoint>,
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// Dashboard summary: total trades, win count, total PnL, max drawdown.
#[allow(clippy::too_many_arguments)]
pub async fn get_summary(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
    account_type: Option<&str>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<SummaryStats> {
    let row = sqlx::query_as::<_, SummaryStats>(
        r#"SELECT
               ds.total_trades,
               ds.win_count,
               ds.total_pnl,
               COALESCE(t.total_fees, 0) AS total_fees,
               ds.max_drawdown
           FROM (
               SELECT
                   COALESCE(SUM(trade_count), 0)::bigint AS total_trades,
                   COALESCE(SUM(win_count), 0)::bigint   AS win_count,
                   COALESCE(SUM(total_pnl), 0)           AS total_pnl,
                   COALESCE(MAX(max_drawdown), 0)         AS max_drawdown
               FROM daily_summary
               WHERE ($1::text IS NULL OR exchange = $1)
                 AND ($2::uuid IS NULL OR paper_account_id = $2)
                 AND ($3::date IS NULL OR date >= $3)
                 AND ($4::date IS NULL OR date <= $4)
                 AND ($5::text IS NULL OR account_type = $5)
           ) ds
           CROSS JOIN LATERAL (
               SELECT COALESCE(SUM(t.fees), 0) AS total_fees
               FROM trades t
               LEFT JOIN paper_accounts pa ON pa.id = t.paper_account_id
               WHERE t.status = 'closed'
                 AND ($1::text IS NULL OR t.exchange = $1)
                 AND ($2::uuid IS NULL OR t.paper_account_id = $2)
                 AND ($3::date IS NULL OR t.exit_at >= $3::date::timestamptz)
                 AND ($4::date IS NULL OR t.exit_at < ($4::date + 1)::timestamptz)
                 AND ($5::text IS NULL OR pa.account_type = $5)
           ) t"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(from)
    .bind(to)
    .bind(account_type)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Daily PnL history with cumulative sum (window function).
#[allow(clippy::too_many_arguments)]
pub async fn get_pnl_history(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
    account_type: Option<&str>,
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
             AND ($5::text IS NULL OR account_type = $5)
           GROUP BY date
           ORDER BY date"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(from)
    .bind(to)
    .bind(account_type)
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
        r#"SELECT t.id, t.strategy_name, t.pair, t.exchange, t.direction,
                  t.entry_price, t.exit_price, t.stop_loss, t.take_profit,
                  t.quantity, t.leverage, t.fees, t.paper_account_id,
                  pa.account_type AS account_type,
                  t.entry_at, t.exit_at, t.pnl_pips, t.pnl_amount,
                  t.exit_reason, t.mode, t.status, t.created_at
           FROM trades t
           LEFT JOIN paper_accounts pa ON pa.id = t.paper_account_id
           WHERE ($1::text IS NULL OR t.exchange = $1)
             AND ($2::uuid IS NULL OR t.paper_account_id = $2)
             AND ($3::text IS NULL OR t.strategy_name = $3)
             AND ($4::text IS NULL OR t.pair = $4)
             AND ($5::text IS NULL OR t.status = $5)
           ORDER BY t.created_at DESC
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

/// Evaluate a paper account's balance.
///
/// After the margin-lock accounting change, `paper_accounts.current_balance`
/// only stores **free cash**. The "total equity" view that the dashboard
/// shows as `evaluated_balance` therefore needs to add back:
///
///   - `locked_margin`: the sum of `quantity × entry_price / leverage`
///     across all currently open crypto trades on this account
///   - `unrealized_pnl`: mark-to-market gain/loss vs the latest candle
///
/// FX trades have `quantity = NULL` and never had margin deducted, so
/// they contribute 0 to `locked_margin` and the legacy unrealized PnL
/// formula (`price_diff * leverage`) still applies.
pub async fn get_evaluated_balance(
    pool: &PgPool,
    paper_account_id: Uuid,
) -> anyhow::Result<EvaluatedBalance> {
    let row = sqlx::query_as::<_, EvaluatedBalance>(
        r#"WITH latest_prices AS (
               SELECT DISTINCT ON (pair, exchange) pair, exchange, close
               FROM price_candles
               ORDER BY pair, exchange, timestamp DESC
           ),
           open_pnl AS (
               -- For crypto (quantity is set): PnL = price_diff * quantity
               -- For FX (quantity is NULL): PnL = price_diff * leverage (matches close_position)
               --
               -- TRUNC is applied **per row** (not on the SUM) so that
               -- the dashboard view matches what `close_position` would
               -- write to the ledger if every open trade closed at its
               -- current mark price. SUM(TRUNC(...)) ≠ TRUNC(SUM(...))
               -- on multi-position accounts (two +0.6 yen positions
               -- should display 0, not 1).
               SELECT
                   t.paper_account_id,
                   SUM(
                       TRUNC(
                           CASE WHEN t.direction = 'long'
                               THEN (COALESCE(lp.close, t.entry_price) - t.entry_price)
                                    * COALESCE(t.quantity, t.leverage)
                               ELSE (t.entry_price - COALESCE(lp.close, t.entry_price))
                                    * COALESCE(t.quantity, t.leverage)
                           END
                       )
                   ) AS unrealized_pnl
               FROM trades t
               LEFT JOIN latest_prices lp
                   ON lp.pair = t.pair AND lp.exchange = t.exchange
               WHERE t.status = 'open' AND t.paper_account_id = $1
               GROUP BY t.paper_account_id
           ),
           locked AS (
               SELECT
                   t.paper_account_id,
                   -- Must match PaperTrader::execute_with_quantity's
                   -- truncate_yen on margin; otherwise the dashboard
                   -- locked-margin view drifts 1 yen per open position.
                   SUM(TRUNC(t.quantity * t.entry_price / t.leverage)) AS locked_margin
               FROM trades t
               WHERE t.status = 'open'
                 AND t.paper_account_id = $1
                 AND t.quantity IS NOT NULL
                 AND t.leverage > 0
               GROUP BY t.paper_account_id
           )
           SELECT
               pa.current_balance,
               -- All three components are already integer-yen
               -- (current_balance via truncate_yen on every write,
               -- locked_margin via per-row TRUNC, unrealized_pnl via
               -- per-row TRUNC inside open_pnl). The sum is therefore
               -- exact and the displayed parts always reconcile with
               -- the displayed total.
               COALESCE(op.unrealized_pnl, 0) AS unrealized_pnl,
               pa.current_balance
                   + COALESCE(lm.locked_margin, 0)
                   + COALESCE(op.unrealized_pnl, 0) AS evaluated_balance
           FROM paper_accounts pa
           LEFT JOIN open_pnl op ON op.paper_account_id = pa.id
           LEFT JOIN locked    lm ON lm.paper_account_id = pa.id
           WHERE pa.id = $1"#,
    )
    .bind(paper_account_id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Daily balance history reconstructed from initial_balance and closed trades.
pub async fn get_balance_history(
    pool: &PgPool,
    account_type: Option<&str>,
) -> anyhow::Result<Vec<BalanceHistoryAccount>> {
    // Load accounts, optionally filtered by account_type.
    let accounts: Vec<(Uuid, String, Decimal)> = sqlx::query_as(
        r#"SELECT id, name, initial_balance
           FROM paper_accounts
           WHERE ($1::text IS NULL OR account_type = $1)
           ORDER BY created_at ASC"#,
    )
    .bind(account_type)
    .fetch_all(pool)
    .await?;

    let mut result = Vec::with_capacity(accounts.len());
    for (account_id, account_name, initial_balance) in accounts {
        let points: Vec<BalanceHistoryPoint> = sqlx::query_as(
            r#"WITH bounds AS (
                   SELECT MIN(DATE(occurred_at)) AS start_date
                   FROM paper_account_events
                   WHERE paper_account_id = $1
               ),
               dates AS (
                   SELECT generate_series(
                       COALESCE((SELECT start_date FROM bounds), CURRENT_DATE),
                       CURRENT_DATE,
                       '1 day'::interval
                   )::date AS date
               ),
               daily_delta AS (
                   SELECT DATE(occurred_at) AS date,
                          SUM(amount) AS daily_net
                   FROM paper_account_events
                   WHERE paper_account_id = $1
                   GROUP BY DATE(occurred_at)
               )
               SELECT
                   d.date,
                   ($2::numeric +
                    COALESCE(
                        SUM(COALESCE(dp.daily_net, 0))
                            OVER (ORDER BY d.date ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW),
                        0
                    )
                   )::numeric AS balance
               FROM dates d
               LEFT JOIN daily_delta dp ON dp.date = d.date
               ORDER BY d.date"#,
        )
        .bind(account_id)
        .bind(initial_balance)
        .fetch_all(pool)
        .await?;

        result.push(BalanceHistoryAccount {
            account_id,
            account_name,
            data: points,
        });
    }
    Ok(result)
}
