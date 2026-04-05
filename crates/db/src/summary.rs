use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;

pub async fn update_daily_max_drawdown(
    pool: &PgPool,
    date: NaiveDate,
) -> anyhow::Result<()> {
    // Get all closed trades for the date, ordered by exit_at
    let rows: Vec<(String, String, String, Decimal)> = sqlx::query_as(
        "SELECT strategy_name, pair, mode, pnl_amount
         FROM trades
         WHERE status = 'closed' AND DATE(exit_at) = $1
         ORDER BY exit_at ASC",
    )
    .bind(date)
    .fetch_all(pool)
    .await?;

    // Group by (strategy, pair, mode) and calculate max drawdown per group
    let mut groups: std::collections::HashMap<(String, String, String), Vec<Decimal>> =
        std::collections::HashMap::new();
    for (strategy, pair, mode, pnl) in rows {
        groups.entry((strategy, pair, mode)).or_default().push(pnl);
    }

    for ((strategy, pair, mode), pnls) in &groups {
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
             WHERE date = $2 AND strategy_name = $3 AND pair = $4 AND mode = $5",
        )
        .bind(max_dd)
        .bind(date)
        .bind(&strategy)
        .bind(&pair)
        .bind(&mode)
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
            sqlx::query(
                r#"INSERT INTO daily_summary (date, strategy_name, pair, mode, trade_count, win_count, total_pnl, max_drawdown)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                   ON CONFLICT (date, strategy_name, pair, mode) DO UPDATE
                   SET max_drawdown = $8"#,
            )
            .bind(date)
            .bind(&strategy)
            .bind(&pair)
            .bind(&mode)
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

pub async fn upsert_daily_summary(
    pool: &PgPool,
    date: NaiveDate,
    strategy_name: &str,
    pair: &str,
    mode: &str,
    trade_count_delta: i32,
    win_count_delta: i32,
    pnl_delta: Decimal,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO daily_summary (date, strategy_name, pair, mode, trade_count, win_count, total_pnl)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           ON CONFLICT (date, strategy_name, pair, mode) DO UPDATE
           SET trade_count = daily_summary.trade_count + $5,
               win_count = daily_summary.win_count + $6,
               total_pnl = daily_summary.total_pnl + $7"#,
    )
    .bind(date)
    .bind(strategy_name)
    .bind(pair)
    .bind(mode)
    .bind(trade_count_delta)
    .bind(win_count_delta)
    .bind(pnl_delta)
    .execute(pool)
    .await?;
    Ok(())
}
