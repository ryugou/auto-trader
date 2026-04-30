use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

/// Row type for the drawdown query: (strategy, pair, exchange, account_id, account_type, pnl).
///
/// `account_id` and `account_type` are non-optional: `trades.account_id` is
/// NOT NULL with an FK to `trading_accounts`, and the query uses INNER JOIN.
/// A row here that can't resolve its account is a DB integrity violation;
/// we let the query error out rather than silently default to "paper".
type DrawdownRow = (String, String, String, Uuid, String, Decimal);

pub async fn update_daily_max_drawdown(pool: &PgPool, date: NaiveDate) -> anyhow::Result<()> {
    // Get all closed trades for the UTC date, ordered by exit_at.
    // INNER JOIN trading_accounts — `trades.account_id` is NOT NULL FK, so
    // a missing account row is DB corruption, not a "maybe-paper" fallback.
    let rows: Vec<DrawdownRow> = sqlx::query_as(
        "SELECT t.strategy_name, t.pair, t.exchange, t.account_id,
                    ta.account_type, t.pnl_amount
             FROM trades t
             INNER JOIN trading_accounts ta ON t.account_id = ta.id
             WHERE t.status = 'closed'
               AND t.exit_at >= ($1::date AT TIME ZONE 'UTC')
               AND t.exit_at < (($1::date + INTERVAL '1 day') AT TIME ZONE 'UTC')
             ORDER BY t.exit_at ASC",
    )
    .bind(date)
    .fetch_all(pool)
    .await?;

    // Group by (strategy, pair, exchange, account_id) and calculate max drawdown per group.
    type GroupKey = (String, String, String, Uuid);
    let mut groups: std::collections::HashMap<GroupKey, (Vec<Decimal>, String)> =
        std::collections::HashMap::new();
    for (strategy, pair, exchange, account_id, account_type, pnl) in rows {
        let entry = groups
            .entry((strategy, pair, exchange, account_id))
            .or_insert_with(|| (Vec::new(), account_type));
        entry.0.push(pnl);
    }

    for ((strategy, pair, exchange, account_id), (pnls, account_type)) in &groups {
        let mut peak = Decimal::ZERO;
        let mut equity = Decimal::ZERO;
        let mut max_dd = Decimal::ZERO;
        for pnl in pnls {
            equity += *pnl;
            if equity > peak {
                peak = equity;
            }
            let dd = peak - equity;
            if dd > max_dd {
                max_dd = dd;
            }
        }

        let account_type_str: &str = account_type;

        // Try to update existing row first.
        let result = sqlx::query(
            "UPDATE daily_summary SET max_drawdown = $1
             WHERE date = $2 AND strategy_name = $3 AND pair = $4 AND account_type = $5
               AND exchange = $6 AND account_id = $7",
        )
        .bind(max_dd)
        .bind(date)
        .bind(strategy.as_str())
        .bind(pair.as_str())
        .bind(account_type_str)
        .bind(exchange.as_str())
        .bind(*account_id)
        .execute(pool)
        .await?;

        if result.rows_affected() == 0 {
            // Row doesn't exist yet — compute actual totals from trades.
            let (total_trades, total_wins, total_pnl): (i64, i64, Decimal) = pnls.iter().fold(
                (0i64, 0i64, Decimal::ZERO),
                |(count, wins, pnl_sum), pnl| {
                    let win = if *pnl > Decimal::ZERO { 1 } else { 0 };
                    (count + 1, wins + win, pnl_sum + *pnl)
                },
            );
            sqlx::query(
                r#"INSERT INTO daily_summary (date, strategy_name, pair, account_type, exchange, account_id, trade_count, win_count, total_pnl, max_drawdown)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                   ON CONFLICT ON CONSTRAINT daily_summary_unique_key DO UPDATE
                   SET max_drawdown = $10"#,
            )
            .bind(date)
            .bind(strategy.as_str())
            .bind(pair.as_str())
            .bind(account_type_str)
            .bind(exchange.as_str())
            .bind(*account_id)
            .bind(total_trades as i32)
            .bind(total_wins as i32)
            .bind(total_pnl)
            .bind(max_dd)
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_daily_summary(
    pool: &PgPool,
    date: NaiveDate,
    strategy_name: &str,
    pair: &str,
    account_type: &str,
    exchange: &str,
    account_id: Option<Uuid>,
    trade_count_delta: i32,
    win_count_delta: i32,
    pnl_delta: Decimal,
) -> anyhow::Result<()> {
    if let Some(aid) = account_id {
        sqlx::query(
            r#"INSERT INTO daily_summary (date, strategy_name, pair, account_type, exchange, account_id, trade_count, win_count, total_pnl)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT ON CONSTRAINT daily_summary_unique_key DO UPDATE
               SET trade_count = daily_summary.trade_count + $7,
                   win_count = daily_summary.win_count + $8,
                   total_pnl = daily_summary.total_pnl + $9"#,
        )
        .bind(date)
        .bind(strategy_name)
        .bind(pair)
        .bind(account_type)
        .bind(exchange)
        .bind(aid)
        .bind(trade_count_delta)
        .bind(win_count_delta)
        .bind(pnl_delta)
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            r#"INSERT INTO daily_summary (date, strategy_name, pair, account_type, exchange, trade_count, win_count, total_pnl)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               ON CONFLICT (date, strategy_name, pair, account_type, exchange) WHERE account_id IS NULL DO UPDATE
               SET trade_count = daily_summary.trade_count + $6,
                   win_count = daily_summary.win_count + $7,
                   total_pnl = daily_summary.total_pnl + $8"#,
        )
        .bind(date)
        .bind(strategy_name)
        .bind(pair)
        .bind(account_type)
        .bind(exchange)
        .bind(trade_count_delta)
        .bind(win_count_delta)
        .bind(pnl_delta)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Regenerate the `daily_summary` cache for the given date from the `trades`
/// source of truth.
///
/// **`date` must be a UTC date** (e.g. `exit_at.date_naive()` where `exit_at`
/// is a `DateTime<Utc>`). Passing a JST-local date will shift the aggregation
/// window by 9 hours and produce incorrect results.
///
/// This is useful when trade records have been retroactively modified or
/// deleted and the incremental summary has drifted. The DELETE + INSERT
/// runs inside a transaction so a failure mid-way does not leave the
/// summary empty. The subsequent max-drawdown recalculation runs outside
/// the transaction — if it fails, rows exist with `max_drawdown = 0` and
/// the function can be safely re-called to retry.
///
/// 1. Deletes all existing `daily_summary` rows for `date`.
/// 2. Re-aggregates from `trades` (joined with `trading_accounts` for
///    `account_type`), grouped by
///    `(date, strategy_name, pair, account_type, exchange, account_id)`.
/// 3. Inserts the aggregated rows with `max_drawdown = 0`.
/// 4. Calls [`update_daily_max_drawdown`] to recalculate the correct
///    max-drawdown values.
pub async fn rebuild_daily_summary(pool: &PgPool, date: NaiveDate) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    // 1. Delete existing rows for this date.
    sqlx::query("DELETE FROM daily_summary WHERE date = $1")
        .bind(date)
        .execute(&mut *tx)
        .await?;

    // 2. Re-insert aggregated rows from trades.
    sqlx::query(
        r#"INSERT INTO daily_summary
               (date, strategy_name, pair, account_type, exchange, account_id,
                trade_count, win_count, total_pnl, max_drawdown)
           SELECT
               $1::date,
               t.strategy_name,
               t.pair,
               ta.account_type,
               t.exchange,
               t.account_id,
               COUNT(*)::int,
               COUNT(*) FILTER (WHERE t.pnl_amount > 0)::int,
               COALESCE(SUM(t.pnl_amount), 0),
               0
           FROM trades t
           INNER JOIN trading_accounts ta ON t.account_id = ta.id
           WHERE t.status = 'closed'
             AND t.exit_at >= ($1::date AT TIME ZONE 'UTC')
             AND t.exit_at <  (($1::date + INTERVAL '1 day') AT TIME ZONE 'UTC')
           GROUP BY t.strategy_name, t.pair, ta.account_type, t.exchange, t.account_id"#,
    )
    .bind(date)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // 3. Recalculate max drawdown (uses its own queries against pool).
    update_daily_max_drawdown(pool, date).await?;

    Ok(())
}

/// Rebuild the `daily_summary` cache for **every** date that has closed trades.
///
/// Iterates over all distinct trade dates and calls [`rebuild_daily_summary`]
/// for each. This is intended for one-off repair / migration scripts, not for
/// hot-path usage.
pub async fn rebuild_all_daily_summaries(pool: &PgPool) -> anyhow::Result<()> {
    let dates: Vec<(NaiveDate,)> = sqlx::query_as(
        "SELECT DISTINCT (exit_at AT TIME ZONE 'UTC')::date AS d
         FROM trades
         WHERE status = 'closed' AND exit_at IS NOT NULL
         ORDER BY d",
    )
    .fetch_all(pool)
    .await?;

    for (date,) in dates {
        rebuild_daily_summary(pool, date).await?;
    }

    Ok(())
}
