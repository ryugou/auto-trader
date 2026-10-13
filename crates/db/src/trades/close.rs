use auto_trader_core::margin::OpenPosition;
use auto_trader_core::types::{ExitReason, Trade};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use super::margin::release_margin;
use super::row::TradeRow;

#[derive(sqlx::FromRow)]
struct AcquireLockRow {
    #[sqlx(flatten)]
    trade: TradeRow,
    old_closing_started_at: Option<DateTime<Utc>>,
}

pub async fn get_trade_for_close<'e, E>(
    executor: E,
    trade_id: Uuid,
    account_id: Uuid,
) -> anyhow::Result<Option<Trade>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, account_id,
                  entry_at, exit_at, pnl_amount,
                  exit_reason, status, created_at, max_hold_until, exchange_position_id,
                  stop_order_id
           FROM trades
           WHERE id = $1 AND account_id = $2 AND status = 'open'"#,
    )
    .bind(trade_id)
    .bind(account_id)
    .fetch_optional(executor)
    .await?;
    row.map(|r| r.try_into()).transpose()
}

pub const STALE_CLOSING_THRESHOLD_SECS: i64 = 300;

pub struct CloseLockAcquired {
    pub trade: Trade,
    pub was_stale_recovery: bool,
}

pub async fn acquire_close_lock(
    pool: &PgPool,
    trade_id: Uuid,
    account_id: Uuid,
) -> anyhow::Result<Option<CloseLockAcquired>> {
    let row = sqlx::query_as::<_, AcquireLockRow>(
        r#"WITH pre AS (
             SELECT closing_started_at AS old_closing_started_at
             FROM trades
             WHERE id = $1 AND account_id = $2
           )
           UPDATE trades
           SET status = 'closing',
               closing_started_at = NOW()
           FROM pre
           WHERE trades.id = $1
             AND trades.account_id = $2
             AND (
               trades.status = 'open'
               OR (
                 trades.status = 'closing'
                 AND trades.closing_started_at IS NOT NULL
                 AND trades.closing_started_at < NOW() - INTERVAL '1 second' * $3
               )
             )
           RETURNING trades.id, trades.strategy_name, trades.pair, trades.exchange,
                     trades.direction, trades.entry_price, trades.exit_price,
                     trades.stop_loss, trades.take_profit, trades.quantity, trades.leverage,
                     trades.fees, trades.account_id,
                     trades.entry_at, trades.exit_at, trades.pnl_amount,
                     trades.exit_reason, trades.status, trades.created_at, trades.max_hold_until,
                     trades.exchange_position_id, trades.stop_order_id,
                     pre.old_closing_started_at"#,
    )
    .bind(trade_id)
    .bind(account_id)
    .bind(STALE_CLOSING_THRESHOLD_SECS)
    .fetch_optional(pool)
    .await?;

    row.map(|r| {
        let was_stale_recovery = r.old_closing_started_at.is_some();
        let trade: Trade = r.trade.try_into()?;
        Ok(CloseLockAcquired {
            trade,
            was_stale_recovery,
        })
    })
    .transpose()
}

pub async fn release_close_lock<'e, E>(executor: E, trade_id: Uuid) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        r#"UPDATE trades
           SET status = 'open',
               closing_started_at = NULL
           WHERE id = $1 AND status = 'closing'"#,
    )
    .bind(trade_id)
    .execute(executor)
    .await?;
    Ok(())
}

pub async fn update_trade_closed<'e, E>(
    executor: E,
    trade_id: Uuid,
    exit_price: Decimal,
    exit_at: DateTime<Utc>,
    pnl_amount: Decimal,
    exit_reason: ExitReason,
    fees: Decimal,
) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let result = sqlx::query(
        r#"UPDATE trades
           SET exit_price = $2,
               exit_at = $3,
               pnl_amount = $4,
               exit_reason = $5,
               fees = $6,
               status = 'closed',
               closing_started_at = NULL
           WHERE id = $1 AND status IN ('open', 'closing')"#,
    )
    .bind(trade_id)
    .bind(exit_price)
    .bind(exit_at)
    .bind(pnl_amount)
    .bind(exit_reason.as_str())
    .bind(fees)
    .execute(executor)
    .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("failed to close trade: not found or already closed");
    }
    Ok(())
}

pub async fn close_trade_reconciled(
    pool: &PgPool,
    trade_id: Uuid,
    exit_price: Decimal,
    pnl_amount: Decimal,
    exit_reason: ExitReason,
) -> anyhow::Result<()> {
    let exit_at = chrono::Utc::now();

    let row = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, account_id,
                  entry_at, exit_at, pnl_amount,
                  exit_reason, status, created_at, max_hold_until, exchange_position_id,
                  stop_order_id
           FROM trades
           WHERE id = $1 AND status IN ('open', 'closing')"#,
    )
    .bind(trade_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("failed to close trade: not found or already closed"))?;
    let trade = Trade::try_from(row)?;

    let margin = OpenPosition {
        direction: trade.direction,
        entry_price: trade.entry_price,
        current_price: trade.entry_price,
        quantity: trade.quantity,
        leverage: trade.leverage,
    }
    .required_margin();

    let mut tx = pool.begin().await?;

    let rows_affected = sqlx::query(
        r#"UPDATE trades
           SET exit_price = $2,
               exit_at = $3,
               pnl_amount = $4,
               exit_reason = $5,
               status = 'closed',
               closing_started_at = NULL
           WHERE id = $1 AND status IN ('open', 'closing')"#,
    )
    .bind(trade_id)
    .bind(exit_price)
    .bind(exit_at)
    .bind(pnl_amount)
    .bind(exit_reason.as_str())
    .execute(&mut *tx)
    .await?
    .rows_affected();

    if rows_affected == 0 {
        anyhow::bail!("failed to close trade: not found or already closed");
    }

    release_margin(&mut tx, trade.account_id, trade_id, margin, pnl_amount).await?;

    tx.commit().await?;
    Ok(())
}
