use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;

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
