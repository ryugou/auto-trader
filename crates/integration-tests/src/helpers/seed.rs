//! Seed helpers for trades, notifications, daily_summary.
//!
//! These complement the existing `db.rs` helpers (which seed accounts).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

/// Insert a closed trade. Returns the trade ID.
#[allow(clippy::too_many_arguments)]
pub async fn seed_closed_trade(
    pool: &PgPool,
    account_id: Uuid,
    strategy_name: &str,
    pair: &str,
    exchange: &str,
    direction: &str,
    entry_price: Decimal,
    exit_price: Decimal,
    pnl_amount: Decimal,
    quantity: Decimal,
    fees: Decimal,
    entry_at: DateTime<Utc>,
    exit_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::new_v4();
    let stop_loss = if direction == "long" {
        entry_price - Decimal::from(100)
    } else {
        entry_price + Decimal::from(100)
    };
    sqlx::query(
        r#"INSERT INTO trades
               (id, account_id, strategy_name, pair, exchange, direction,
                entry_price, exit_price, stop_loss, quantity, leverage,
                fees, pnl_amount, exit_reason, status, entry_at, exit_at)
           VALUES ($1, $2, $3, $4, $5, $6,
                   $7, $8, $9, $10, 2,
                   $11, $12, 'sl_hit', 'closed', $13, $14)"#,
    )
    .bind(id)
    .bind(account_id)
    .bind(strategy_name)
    .bind(pair)
    .bind(exchange)
    .bind(direction)
    .bind(entry_price)
    .bind(exit_price)
    .bind(stop_loss)
    .bind(quantity)
    .bind(fees)
    .bind(pnl_amount)
    .bind(entry_at)
    .bind(exit_at)
    .execute(pool)
    .await
    .expect("seed_closed_trade: insert failed");
    id
}

/// Insert an open trade. Returns the trade ID.
#[allow(clippy::too_many_arguments)]
pub async fn seed_open_trade(
    pool: &PgPool,
    account_id: Uuid,
    strategy_name: &str,
    pair: &str,
    exchange: &str,
    direction: &str,
    entry_price: Decimal,
    stop_loss: Decimal,
    quantity: Decimal,
    entry_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO trades
               (id, account_id, strategy_name, pair, exchange, direction,
                entry_price, stop_loss, quantity, leverage,
                fees, status, entry_at)
           VALUES ($1, $2, $3, $4, $5, $6,
                   $7, $8, $9, 2,
                   0, 'open', $10)"#,
    )
    .bind(id)
    .bind(account_id)
    .bind(strategy_name)
    .bind(pair)
    .bind(exchange)
    .bind(direction)
    .bind(entry_price)
    .bind(stop_loss)
    .bind(quantity)
    .bind(entry_at)
    .execute(pool)
    .await
    .expect("seed_open_trade: insert failed");
    id
}

/// Insert a notification row. Returns the notification ID.
#[allow(clippy::too_many_arguments)]
pub async fn seed_notification(
    pool: &PgPool,
    kind: &str,
    trade_id: Uuid,
    account_id: Uuid,
    strategy_name: &str,
    pair: &str,
    direction: &str,
    price: Decimal,
    pnl_amount: Option<Decimal>,
    exit_reason: Option<&str>,
    read_at: Option<DateTime<Utc>>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO notifications
               (id, kind, trade_id, account_id, strategy_name, pair,
                direction, price, pnl_amount, exit_reason, read_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
    )
    .bind(id)
    .bind(kind)
    .bind(trade_id)
    .bind(account_id)
    .bind(strategy_name)
    .bind(pair)
    .bind(direction)
    .bind(price)
    .bind(pnl_amount)
    .bind(exit_reason)
    .bind(read_at)
    .execute(pool)
    .await
    .expect("seed_notification: insert failed");
    id
}

/// Insert a daily_summary row.
#[allow(clippy::too_many_arguments)]
pub async fn seed_daily_summary(
    pool: &PgPool,
    account_id: Uuid,
    date: chrono::NaiveDate,
    strategy_name: &str,
    pair: &str,
    exchange: &str,
    account_type: &str,
    trade_count: i64,
    win_count: i64,
    total_pnl: Decimal,
    max_drawdown: Decimal,
) {
    sqlx::query(
        r#"INSERT INTO daily_summary
               (account_id, date, strategy_name, pair, exchange,
                account_type, trade_count, win_count, total_pnl, max_drawdown)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
    )
    .bind(account_id)
    .bind(date)
    .bind(strategy_name)
    .bind(pair)
    .bind(exchange)
    .bind(account_type)
    .bind(trade_count)
    .bind(win_count)
    .bind(total_pnl)
    .bind(max_drawdown)
    .execute(pool)
    .await
    .expect("seed_daily_summary: insert failed");
}
