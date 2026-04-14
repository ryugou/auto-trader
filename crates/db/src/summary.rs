use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn update_daily_max_drawdown(pool: &PgPool, date: NaiveDate) -> anyhow::Result<()> {
    // Get all closed trades for the UTC date, ordered by exit_at
    let rows: Vec<(String, String, String, String, Option<Uuid>, Decimal)> = sqlx::query_as(
        "SELECT strategy_name, pair, mode, exchange, paper_account_id, pnl_amount
         FROM trades
         WHERE status = 'closed'
           AND exit_at >= ($1::date AT TIME ZONE 'UTC')
           AND exit_at < (($1::date + INTERVAL '1 day') AT TIME ZONE 'UTC')
         ORDER BY exit_at ASC",
    )
    .bind(date)
    .fetch_all(pool)
    .await?;

    // Group by (strategy, pair, mode, exchange, paper_account_id) and calculate max drawdown per group
    type GroupKey = (String, String, String, String, Option<Uuid>);
    let mut groups: std::collections::HashMap<GroupKey, Vec<Decimal>> =
        std::collections::HashMap::new();
    for (strategy, pair, mode, exchange, paper_account_id, pnl) in rows {
        groups
            .entry((strategy, pair, mode, exchange, paper_account_id))
            .or_default()
            .push(pnl);
    }

    for ((strategy, pair, mode, exchange, paper_account_id), pnls) in &groups {
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

        // Ensure daily_summary row exists before updating max_drawdown
        let result = sqlx::query(
            "UPDATE daily_summary SET max_drawdown = $1
             WHERE date = $2 AND strategy_name = $3 AND pair = $4 AND mode = $5
               AND exchange = $6 AND paper_account_id IS NOT DISTINCT FROM $7",
        )
        .bind(max_dd)
        .bind(date)
        .bind(strategy.as_str())
        .bind(pair.as_str())
        .bind(mode.as_str())
        .bind(exchange.as_str())
        .bind(*paper_account_id)
        .execute(pool)
        .await?;

        if result.rows_affected() == 0 {
            // Row doesn't exist yet — compute actual totals from trades
            let (total_trades, total_wins, total_pnl): (i64, i64, Decimal) = pnls.iter().fold(
                (0i64, 0i64, Decimal::ZERO),
                |(count, wins, pnl_sum), pnl| {
                    let win = if *pnl > Decimal::ZERO { 1 } else { 0 };
                    (count + 1, wins + win, pnl_sum + *pnl)
                },
            );
            if let Some(account_id) = paper_account_id {
                sqlx::query(
                    r#"INSERT INTO daily_summary (date, strategy_name, pair, mode, exchange, paper_account_id, trade_count, win_count, total_pnl, max_drawdown)
                       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                       ON CONFLICT ON CONSTRAINT daily_summary_unique_key DO UPDATE
                       SET max_drawdown = $10"#,
                )
                .bind(date)
                .bind(strategy.as_str())
                .bind(pair.as_str())
                .bind(mode.as_str())
                .bind(exchange.as_str())
                .bind(account_id)
                .bind(total_trades as i32)
                .bind(total_wins as i32)
                .bind(total_pnl)
                .bind(max_dd)
                .execute(pool)
                .await?;
            } else {
                sqlx::query(
                    r#"INSERT INTO daily_summary (date, strategy_name, pair, mode, exchange, trade_count, win_count, total_pnl, max_drawdown)
                       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                       ON CONFLICT (date, strategy_name, pair, mode, exchange) WHERE paper_account_id IS NULL DO UPDATE
                       SET max_drawdown = $9"#,
                )
                .bind(date)
                .bind(strategy.as_str())
                .bind(pair.as_str())
                .bind(mode.as_str())
                .bind(exchange.as_str())
                .bind(total_trades as i32)
                .bind(total_wins as i32)
                .bind(total_pnl)
                .bind(max_dd)
                .execute(pool)
                .await?;
            }
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
    mode: &str,
    exchange: &str,
    paper_account_id: Option<Uuid>,
    account_type: Option<&str>,
    trade_count_delta: i32,
    win_count_delta: i32,
    pnl_delta: Decimal,
) -> anyhow::Result<()> {
    if let Some(account_id) = paper_account_id {
        // Crypto path: paper_account_id is NOT NULL, use main UNIQUE constraint
        sqlx::query(
            r#"INSERT INTO daily_summary (date, strategy_name, pair, mode, exchange, paper_account_id, account_type, trade_count, win_count, total_pnl)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
               ON CONFLICT ON CONSTRAINT daily_summary_unique_key DO UPDATE
               SET trade_count = daily_summary.trade_count + $8,
                   win_count = daily_summary.win_count + $9,
                   total_pnl = daily_summary.total_pnl + $10,
                   account_type = COALESCE(EXCLUDED.account_type, daily_summary.account_type)"#,
        )
        .bind(date)
        .bind(strategy_name)
        .bind(pair)
        .bind(mode)
        .bind(exchange)
        .bind(account_id)
        .bind(account_type)
        .bind(trade_count_delta)
        .bind(win_count_delta)
        .bind(pnl_delta)
        .execute(pool)
        .await?;
    } else {
        // FX path: paper_account_id IS NULL, use partial unique index
        sqlx::query(
            r#"INSERT INTO daily_summary (date, strategy_name, pair, mode, exchange, account_type, trade_count, win_count, total_pnl)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT (date, strategy_name, pair, mode, exchange) WHERE paper_account_id IS NULL DO UPDATE
               SET trade_count = daily_summary.trade_count + $7,
                   win_count = daily_summary.win_count + $8,
                   total_pnl = daily_summary.total_pnl + $9,
                   account_type = COALESCE(EXCLUDED.account_type, daily_summary.account_type)"#,
        )
        .bind(date)
        .bind(strategy_name)
        .bind(pair)
        .bind(mode)
        .bind(exchange)
        .bind(account_type)
        .bind(trade_count_delta)
        .bind(win_count_delta)
        .bind(pnl_delta)
        .execute(pool)
        .await?;
    }
    Ok(())
}
