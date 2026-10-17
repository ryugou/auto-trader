use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

/// Trade statistics summed over the daily summary rows that match the filters.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SummaryStats {
    /// Number of trades.
    pub total_trades: i64,
    /// Number of winning trades.
    pub win_count: i64,
    /// Total profit or loss.
    pub total_pnl: Decimal,
    /// Total fees of the closed trades that match the filters.
    pub total_fees: Decimal,
    /// Largest maximum drawdown among the summarized rows.
    pub max_drawdown: Decimal,
}

/// Profit or loss of one day and its running total.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PnlHistoryRow {
    /// Day of the summary.
    pub date: NaiveDate,
    /// Profit or loss of the day.
    pub daily_pnl: Decimal,
    /// Sum of the profit or loss from the first day up to this day.
    pub cumulative_pnl: Decimal,
}

/// Trade statistics of one strategy.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct StrategyStats {
    /// Name of the strategy.
    pub strategy_name: String,
    /// Number of trades.
    pub trade_count: i64,
    /// Number of winning trades.
    pub win_count: i64,
    /// Total profit or loss.
    pub total_pnl: Decimal,
}

/// Trade statistics of one currency pair.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PairStats {
    /// Currency pair.
    pub pair: String,
    /// Number of trades.
    pub trade_count: i64,
    /// Number of winning trades.
    pub win_count: i64,
    /// Total profit or loss.
    pub total_pnl: Decimal,
}

/// Trade statistics of one hour of the day.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct HourlyWinrate {
    /// Hour of the entry time, from 0 to 23.
    pub hour: i32,
    /// Number of closed trades entered in this hour.
    pub trade_count: i64,
    /// Number of winning trades among them.
    pub win_count: i64,
}

/// Trade as shown on the dashboard, with the type of its account.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TradeRow {
    /// Identifier of the trade.
    pub id: Uuid,
    /// Name of the strategy.
    pub strategy_name: String,
    /// Currency pair.
    pub pair: String,
    /// Exchange name as text.
    pub exchange: String,
    /// Direction of the trade as text.
    pub direction: String,
    /// Entry price.
    pub entry_price: Decimal,
    /// Exit price, if the trade has one.
    pub exit_price: Option<Decimal>,
    /// Stop-loss price.
    pub stop_loss: Decimal,
    /// Take-profit price, if the trade has one.
    pub take_profit: Option<Decimal>,
    /// Quantity of the trade.
    pub quantity: Decimal,
    /// Leverage of the trade.
    pub leverage: Decimal,
    /// Fees charged to the trade.
    pub fees: Decimal,
    /// Identifier of the account that holds the trade.
    pub account_id: Uuid,
    /// Type of the account that holds the trade.
    pub account_type: Option<String>,
    /// Time of the entry.
    pub entry_at: DateTime<Utc>,
    /// Time of the exit, if the trade has one.
    pub exit_at: Option<DateTime<Utc>>,
    /// Profit or loss, if recorded.
    pub pnl_amount: Option<Decimal>,
    /// Reason the trade was closed, if any.
    pub exit_reason: Option<String>,
    /// Status of the trade as text.
    pub status: String,
    /// Time the trade row was created.
    pub created_at: DateTime<Utc>,
}

/// Balance of an account valued with its open trades.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EvaluatedBalance {
    /// Current balance of the account.
    pub current_balance: Decimal,
    /// Sum of the unrealized profit or loss of the open trades, each truncated to a whole number and valued at the latest candle close of its pair and exchange, or at its entry price when there is none.
    pub unrealized_pnl: Decimal,
    /// Current balance plus the margin locked by the open trades plus `unrealized_pnl`.
    pub evaluated_balance: Decimal,
}

/// Balance of an account at the end of one day.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct BalanceHistoryPoint {
    /// Day of the balance.
    pub date: NaiveDate,
    /// Balance at the end of the day.
    pub balance: Decimal,
}

/// Daily balance history of one account.
#[derive(Debug, Serialize)]
pub struct BalanceHistoryAccount {
    /// Identifier of the account.
    pub account_id: Uuid,
    /// Name of the account.
    pub account_name: String,
    /// Balance of each day from the day of the first account event up to today, in date order.
    pub data: Vec<BalanceHistoryPoint>,
}

/// Returns the trade counts, win counts, profit or loss and maximum drawdown of the daily summary rows that match the filters, together with the fees of the closed trades that match them. A filter left as `None` is not applied, and `from` and `to` bound the day inclusively.
///
/// # Errors
///
/// - Returns an error if the database query fails.
#[allow(clippy::too_many_arguments)]
pub async fn get_summary(
    pool: &PgPool,
    exchange: Option<&str>,
    account_id: Option<Uuid>,
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
                 AND ($2::uuid IS NULL OR account_id = $2)
                 AND ($3::date IS NULL OR date >= $3)
                 AND ($4::date IS NULL OR date <= $4)
                 AND ($5::text IS NULL OR account_type = $5)
           ) ds
           CROSS JOIN LATERAL (
               SELECT COALESCE(SUM(t.fees), 0) AS total_fees
               FROM trades t
               JOIN trading_accounts ta ON ta.id = t.account_id
               WHERE t.status = 'closed'
                 AND ($1::text IS NULL OR t.exchange = $1)
                 AND ($2::uuid IS NULL OR t.account_id = $2)
                 AND ($3::date IS NULL OR t.exit_at >= $3::date::timestamptz)
                 AND ($4::date IS NULL OR t.exit_at < ($4::date + 1)::timestamptz)
                 AND ($5::text IS NULL OR ta.account_type = $5)
           ) t"#,
    )
    .bind(exchange)
    .bind(account_id)
    .bind(from)
    .bind(to)
    .bind(account_type)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Returns the profit or loss of each day with its running total, in date order, over the daily summary rows that match the filters. A filter left as `None` is not applied, and `from` and `to` bound the day inclusively.
///
/// # Errors
///
/// - Returns an error if the database query fails.
#[allow(clippy::too_many_arguments)]
pub async fn get_pnl_history(
    pool: &PgPool,
    exchange: Option<&str>,
    account_id: Option<Uuid>,
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
             AND ($2::uuid IS NULL OR account_id = $2)
             AND ($3::date IS NULL OR date >= $3)
             AND ($4::date IS NULL OR date <= $4)
             AND ($5::text IS NULL OR account_type = $5)
           GROUP BY date
           ORDER BY date"#,
    )
    .bind(exchange)
    .bind(account_id)
    .bind(from)
    .bind(to)
    .bind(account_type)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Returns the trade count, win count and profit or loss of each strategy, largest profit or loss first, over the daily summary rows that match the filters. A filter left as `None` is not applied.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn get_strategy_stats(
    pool: &PgPool,
    exchange: Option<&str>,
    account_id: Option<Uuid>,
) -> anyhow::Result<Vec<StrategyStats>> {
    let rows = sqlx::query_as::<_, StrategyStats>(
        r#"SELECT
               strategy_name,
               COALESCE(SUM(trade_count), 0)::bigint AS trade_count,
               COALESCE(SUM(win_count), 0)::bigint   AS win_count,
               COALESCE(SUM(total_pnl), 0)           AS total_pnl
           FROM daily_summary
           WHERE ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR account_id = $2)
           GROUP BY strategy_name
           ORDER BY total_pnl DESC"#,
    )
    .bind(exchange)
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Returns the trade count, win count and profit or loss of each currency pair, largest profit or loss first, over the daily summary rows that match the filters. A filter left as `None` is not applied.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn get_pair_stats(
    pool: &PgPool,
    exchange: Option<&str>,
    account_id: Option<Uuid>,
) -> anyhow::Result<Vec<PairStats>> {
    let rows = sqlx::query_as::<_, PairStats>(
        r#"SELECT
               pair,
               COALESCE(SUM(trade_count), 0)::bigint AS trade_count,
               COALESCE(SUM(win_count), 0)::bigint   AS win_count,
               COALESCE(SUM(total_pnl), 0)           AS total_pnl
           FROM daily_summary
           WHERE ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR account_id = $2)
           GROUP BY pair
           ORDER BY total_pnl DESC"#,
    )
    .bind(exchange)
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Returns the number of closed trades and the number of winning trades for each hour of the entry time, in hour order. A filter left as `None` is not applied.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn get_hourly_winrate(
    pool: &PgPool,
    exchange: Option<&str>,
    account_id: Option<Uuid>,
) -> anyhow::Result<Vec<HourlyWinrate>> {
    let rows = sqlx::query_as::<_, HourlyWinrate>(
        r#"SELECT
               EXTRACT(HOUR FROM entry_at)::int AS hour,
               COUNT(*)::bigint                 AS trade_count,
               COUNT(*) FILTER (WHERE pnl_amount > 0)::bigint AS win_count
           FROM trades
           WHERE status = 'closed'
             AND ($1::text IS NULL OR exchange = $1)
             AND ($2::uuid IS NULL OR account_id = $2)
           GROUP BY hour
           ORDER BY hour"#,
    )
    .bind(exchange)
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Returns a page of the trades that match the filters, newest created first, together with the number of matching trades. A filter left as `None` is not applied. `page` counts from 1 and defaults to 1, and `per_page` defaults to 50 and is capped at 200.
///
/// # Errors
///
/// - Returns an error if the database query fails.
#[allow(clippy::too_many_arguments)]
pub async fn get_trades(
    pool: &PgPool,
    exchange: Option<&str>,
    account_id: Option<Uuid>,
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
             AND ($2::uuid IS NULL OR account_id = $2)
             AND ($3::text IS NULL OR strategy_name = $3)
             AND ($4::text IS NULL OR pair = $4)
             AND ($5::text IS NULL OR status = $5)"#,
    )
    .bind(exchange)
    .bind(account_id)
    .bind(strategy)
    .bind(pair)
    .bind(status)
    .fetch_one(pool)
    .await?;

    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT t.id, t.strategy_name, t.pair, t.exchange, t.direction,
                  t.entry_price, t.exit_price, t.stop_loss, t.take_profit,
                  t.quantity, t.leverage, t.fees, t.account_id,
                  ta.account_type AS account_type,
                  t.entry_at, t.exit_at, t.pnl_amount,
                  t.exit_reason, t.status, t.created_at
           FROM trades t
           JOIN trading_accounts ta ON ta.id = t.account_id
           WHERE ($1::text IS NULL OR t.exchange = $1)
             AND ($2::uuid IS NULL OR t.account_id = $2)
             AND ($3::text IS NULL OR t.strategy_name = $3)
             AND ($4::text IS NULL OR t.pair = $4)
             AND ($5::text IS NULL OR t.status = $5)
           ORDER BY t.created_at DESC
           LIMIT $6 OFFSET $7"#,
    )
    .bind(exchange)
    .bind(account_id)
    .bind(strategy)
    .bind(pair)
    .bind(status)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    Ok((rows, total.0))
}

/// Returns the evaluated balance of the account.
///
/// # Errors
///
/// - Returns an error if the database query fails, including when the account does not exist.
pub async fn get_evaluated_balance(
    pool: &PgPool,
    account_id: Uuid,
) -> anyhow::Result<EvaluatedBalance> {
    let row = sqlx::query_as::<_, EvaluatedBalance>(
        r#"WITH latest_prices AS (
               SELECT DISTINCT ON (pair, exchange) pair, exchange, close
               FROM price_candles
               ORDER BY pair, exchange, timestamp DESC
           ),
           open_pnl AS (
               SELECT
                   t.account_id,
                   SUM(
                       TRUNC(
                           CASE WHEN t.direction = 'long'
                               THEN (COALESCE(lp.close, t.entry_price) - t.entry_price)
                                    * t.quantity
                               ELSE (t.entry_price - COALESCE(lp.close, t.entry_price))
                                    * t.quantity
                           END
                       )
                   ) AS unrealized_pnl
               FROM trades t
               LEFT JOIN latest_prices lp
                   ON lp.pair = t.pair AND lp.exchange = t.exchange
               WHERE t.status = 'open' AND t.account_id = $1
               GROUP BY t.account_id
           ),
           locked AS (
               SELECT
                   t.account_id,
                   SUM(TRUNC(t.quantity * t.entry_price / t.leverage)) AS locked_margin
               FROM trades t
               WHERE t.status = 'open'
                 AND t.account_id = $1
                 AND t.leverage > 0
               GROUP BY t.account_id
           )
           SELECT
               ta.current_balance,
               COALESCE(op.unrealized_pnl, 0) AS unrealized_pnl,
               ta.current_balance
                   + COALESCE(lm.locked_margin, 0)
                   + COALESCE(op.unrealized_pnl, 0) AS evaluated_balance
           FROM trading_accounts ta
           LEFT JOIN open_pnl op ON op.account_id = ta.id
           LEFT JOIN locked    lm ON lm.account_id = ta.id
           WHERE ta.id = $1"#,
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Returns the evaluated balance of every account, keyed by the identifier of the account.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn get_all_evaluated_balances(
    pool: &PgPool,
) -> anyhow::Result<std::collections::HashMap<uuid::Uuid, EvaluatedBalance>> {
    let rows: Vec<(uuid::Uuid, Decimal, Decimal, Decimal)> = sqlx::query_as(
        r#"WITH latest_prices AS (
               SELECT DISTINCT ON (pair, exchange) pair, exchange, close
               FROM price_candles
               ORDER BY pair, exchange, timestamp DESC
           ),
           open_pnl AS (
               SELECT
                   t.account_id,
                   SUM(
                       TRUNC(
                           CASE WHEN t.direction = 'long'
                               THEN (COALESCE(lp.close, t.entry_price) - t.entry_price)
                                    * t.quantity
                               ELSE (t.entry_price - COALESCE(lp.close, t.entry_price))
                                    * t.quantity
                           END
                       )
                   ) AS unrealized_pnl
               FROM trades t
               LEFT JOIN latest_prices lp
                   ON lp.pair = t.pair AND lp.exchange = t.exchange
               WHERE t.status = 'open'
               GROUP BY t.account_id
           ),
           locked AS (
               SELECT
                   t.account_id,
                   SUM(TRUNC(t.quantity * t.entry_price / t.leverage)) AS locked_margin
               FROM trades t
               WHERE t.status = 'open' AND t.leverage > 0
               GROUP BY t.account_id
           )
           SELECT
               ta.id,
               ta.current_balance,
               COALESCE(op.unrealized_pnl, 0) AS unrealized_pnl,
               ta.current_balance
                   + COALESCE(lm.locked_margin, 0)
                   + COALESCE(op.unrealized_pnl, 0) AS evaluated_balance
           FROM trading_accounts ta
           LEFT JOIN open_pnl op ON op.account_id = ta.id
           LEFT JOIN locked    lm ON lm.account_id = ta.id"#,
    )
    .fetch_all(pool)
    .await?;

    let map = rows
        .into_iter()
        .map(|(id, current_balance, unrealized_pnl, evaluated_balance)| {
            (
                id,
                EvaluatedBalance {
                    current_balance,
                    unrealized_pnl,
                    evaluated_balance,
                },
            )
        })
        .collect();
    Ok(map)
}

/// Returns the daily balance history of each active account, oldest account first. When `account_type` is given, only accounts of that type are returned. The balance of a day is the initial balance plus the sum of the trade close, overnight fee, SFD fee and swap fee events up to that day.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn get_balance_history(
    pool: &PgPool,
    account_type: Option<&str>,
) -> anyhow::Result<Vec<BalanceHistoryAccount>> {
    let accounts: Vec<(Uuid, String, Decimal)> = sqlx::query_as(
        r#"SELECT id, name, initial_balance
           FROM trading_accounts
           WHERE active
             AND ($1::text IS NULL OR account_type = $1)
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
                   FROM account_events
                   WHERE account_id = $1
               ),
               dates AS (
                   SELECT generate_series(
                       COALESCE((SELECT start_date FROM bounds), CURRENT_DATE),
                       CURRENT_DATE,
                       '1 day'::interval
                   )::date AS date
               ),
               daily_delta AS (
                   -- Only realized P&L events (trade_close + overnight_fee + sfd_fee + swap_fee).
                   -- Margin lock/release are excluded so the chart shows
                   -- the account's true value growth, not the cash dips
                   -- from opening positions.
                   -- sfd_fee は bitFlyer Crypto CFD の SFD (PR #92)、
                   -- swap_fee は GMO FX paper accrual (本 PR)。
                   -- どちらも `apply_*_fee` で符号両対応で記録される。
                   SELECT DATE(occurred_at) AS date,
                          SUM(amount) AS daily_net
                   FROM account_events
                   WHERE account_id = $1
                     AND event_type IN ('trade_close', 'overnight_fee', 'sfd_fee', 'swap_fee')
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
