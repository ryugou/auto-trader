use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Trade, TradeStatus};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct AcquireLockRow {
    #[sqlx(flatten)]
    trade: TradeRow,
    old_closing_started_at: Option<DateTime<Utc>>,
}

pub async fn insert_trade<'e, E>(executor: E, trade: &Trade) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let direction = trade.direction.as_str();
    let status = trade.status.as_str();
    sqlx::query(
        r#"INSERT INTO trades
               (id, account_id, strategy_name, pair, exchange, direction,
                entry_price, exit_price, stop_loss, take_profit,
                quantity, leverage, fees, entry_at, exit_at,
                pnl_amount, exit_reason, status, max_hold_until,
                exchange_position_id, stop_order_id)
           VALUES ($1, $2, $3, $4, $5, $6,
                   $7, $8, $9, $10,
                   $11, $12, $13, $14, $15,
                   $16, $17, $18, $19, $20, $21)"#,
    )
    .bind(trade.id)
    .bind(trade.account_id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(trade.exchange.as_str())
    .bind(direction)
    .bind(trade.entry_price)
    .bind(trade.exit_price)
    .bind(trade.stop_loss)
    .bind(trade.take_profit)
    .bind(trade.quantity)
    .bind(trade.leverage)
    .bind(trade.fees)
    .bind(trade.entry_at)
    .bind(trade.exit_at)
    .bind(trade.pnl_amount)
    .bind(trade.exit_reason.map(|r| r.as_str()))
    .bind(status)
    .bind(trade.max_hold_until)
    .bind(&trade.exchange_position_id)
    .bind(&trade.stop_order_id)
    .execute(executor)
    .await?;
    Ok(())
}

pub async fn lock_margin(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    margin_amount: Decimal,
) -> anyhow::Result<()> {
    let new_balance: Decimal = sqlx::query_scalar(
        r#"UPDATE trading_accounts
           SET current_balance = current_balance - $2
           WHERE id = $1
           RETURNING current_balance"#,
    )
    .bind(account_id)
    .bind(margin_amount)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
           VALUES ($1, $2, 'margin_lock', $3, $4)"#,
    )
    .bind(account_id)
    .bind(trade_id)
    .bind(-margin_amount)
    .bind(new_balance)
    .execute(&mut *tx)
    .await?;
    Ok(())
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

async fn apply_fee_inner(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
    event_type: &'static str,
    occurred_at: Option<DateTime<Utc>>,
) -> anyhow::Result<Option<Decimal>> {
    let trade_updated = sqlx::query(
        "UPDATE trades SET fees = fees + $3
         WHERE id = $1 AND account_id = $2 AND status = 'open'",
    )
    .bind(trade_id)
    .bind(account_id)
    .bind(fee_amount)
    .execute(&mut *tx)
    .await?;

    if trade_updated.rows_affected() == 0 {
        return Ok(None);
    }

    let new_balance: Decimal = sqlx::query_scalar(
        r#"UPDATE trading_accounts
           SET current_balance = current_balance - $2
           WHERE id = $1
           RETURNING current_balance"#,
    )
    .bind(account_id)
    .bind(fee_amount)
    .fetch_one(&mut *tx)
    .await?;

    match occurred_at {
        Some(at) => {
            sqlx::query(
                r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after, occurred_at)
                   VALUES ($1, $2, $3, $4, $5, $6)"#,
            )
            .bind(account_id)
            .bind(trade_id)
            .bind(event_type)
            .bind(-fee_amount)
            .bind(new_balance)
            .bind(at)
            .execute(&mut *tx)
            .await?;
        }
        None => {
            sqlx::query(
                r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
                   VALUES ($1, $2, $3, $4, $5)"#,
            )
            .bind(account_id)
            .bind(trade_id)
            .bind(event_type)
            .bind(-fee_amount)
            .bind(new_balance)
            .execute(&mut *tx)
            .await?;
        }
    }

    Ok(Some(new_balance))
}

pub async fn apply_overnight_fee(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
) -> anyhow::Result<Option<Decimal>> {
    apply_fee_inner(tx, account_id, trade_id, fee_amount, "overnight_fee", None).await
}

pub async fn apply_sfd_fee(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
    occurred_at: DateTime<Utc>,
) -> anyhow::Result<Option<Decimal>> {
    apply_fee_inner(
        tx,
        account_id,
        trade_id,
        fee_amount,
        "sfd_fee",
        Some(occurred_at),
    )
    .await
}

pub async fn apply_swap_fee(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
    occurred_at: DateTime<Utc>,
) -> anyhow::Result<Option<Decimal>> {
    apply_fee_inner(
        tx,
        account_id,
        trade_id,
        fee_amount,
        "swap_fee",
        Some(occurred_at),
    )
    .await
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

pub async fn release_margin(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    margin_return: Decimal,
    pnl_amount: Decimal,
) -> anyhow::Result<()> {
    let new_balance: Decimal = sqlx::query_scalar(
        r#"UPDATE trading_accounts
           SET current_balance = current_balance + $2 + $3
           WHERE id = $1
           RETURNING current_balance"#,
    )
    .bind(account_id)
    .bind(margin_return)
    .bind(pnl_amount)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
           VALUES ($1, $2, 'margin_release', $3, $4)"#,
    )
    .bind(account_id)
    .bind(trade_id)
    .bind(margin_return)
    .bind(new_balance - pnl_amount)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
           VALUES ($1, $2, 'trade_close', $3, $4)"#,
    )
    .bind(account_id)
    .bind(trade_id)
    .bind(pnl_amount)
    .bind(new_balance)
    .execute(&mut *tx)
    .await?;

    Ok(())
}

pub async fn list_open_or_closing_by_account(
    pool: &PgPool,
    account_id: Uuid,
) -> anyhow::Result<Vec<Trade>> {
    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, account_id,
                  entry_at, exit_at, pnl_amount,
                  exit_reason, status, created_at, max_hold_until, exchange_position_id,
                  stop_order_id
           FROM trades
           WHERE account_id = $1 AND status IN ('open', 'closing')
           ORDER BY entry_at DESC"#,
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|r| r.try_into()).collect()
}

pub async fn close_trade_reconciled(
    pool: &PgPool,
    trade_id: Uuid,
    exit_price: Decimal,
    pnl_amount: Decimal,
    exit_reason: ExitReason,
) -> anyhow::Result<()> {
    let exit_at = chrono::Utc::now();

    let trade = sqlx::query_as::<_, TradeRow>(
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

    let margin = (trade.entry_price * trade.quantity / trade.leverage)
        .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);

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

pub async fn get_open_trades_by_account(
    pool: &PgPool,
    account_id: Uuid,
) -> anyhow::Result<Vec<Trade>> {
    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, account_id,
                  entry_at, exit_at, pnl_amount,
                  exit_reason, status, created_at, max_hold_until, exchange_position_id,
                  stop_order_id
           FROM trades
           WHERE account_id = $1 AND status = 'open'
           ORDER BY entry_at DESC"#,
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|r| r.try_into()).collect()
}

pub async fn get_open_trades(
    pool: &PgPool,
    strategy_name: &str,
    pair: &str,
) -> anyhow::Result<Vec<Trade>> {
    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, account_id,
                  entry_at, exit_at, pnl_amount,
                  exit_reason, status, created_at, max_hold_until, exchange_position_id,
                  stop_order_id
           FROM trades
           WHERE strategy_name = $1 AND pair = $2 AND status = 'open'"#,
    )
    .bind(strategy_name)
    .bind(pair)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|r| r.try_into()).collect()
}

pub async fn get_trade_by_id(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<Trade>> {
    let row = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, account_id,
                  entry_at, exit_at, pnl_amount,
                  exit_reason, status, created_at, max_hold_until, exchange_position_id,
                  stop_order_id
           FROM trades
           WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(|r| r.try_into()).transpose()
}

pub async fn get_open_trade_with_account(
    pool: &PgPool,
    trade_id: Uuid,
) -> anyhow::Result<Option<OpenTradeWithAccount>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        trade: TradeRow,
        account_name: Option<String>,
        account_type: Option<String>,
    }
    let row = sqlx::query_as::<_, Row>(
        r#"SELECT t.id, t.strategy_name, t.pair, t.exchange, t.direction, t.entry_price, t.exit_price,
                  t.stop_loss, t.take_profit, t.quantity, t.leverage, t.fees, t.account_id,
                  t.entry_at, t.exit_at, t.pnl_amount,
                  t.exit_reason, t.status, t.created_at, t.max_hold_until, t.exchange_position_id,
                  t.stop_order_id,
                  ta.name AS account_name, ta.account_type AS account_type
           FROM trades t
           LEFT JOIN trading_accounts ta ON t.account_id = ta.id
           WHERE t.id = $1 AND t.status = 'open'"#,
    )
    .bind(trade_id)
    .fetch_optional(pool)
    .await?;
    row.map(|r| {
        let trade: Trade = r.trade.try_into()?;
        Ok(OpenTradeWithAccount {
            trade,
            account_name: r.account_name,
            account_type: r.account_type,
        })
    })
    .transpose()
}

pub async fn add_fees(pool: &PgPool, id: Uuid, fee_delta: Decimal) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE trades SET fees = fees + $2 WHERE id = $1")
        .bind(id)
        .bind(fee_delta)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("failed to add fees for trade: not found");
    }
    Ok(())
}

pub async fn realized_net_since(
    pool: &PgPool,
    account_id: Uuid,
    since: DateTime<Utc>,
) -> anyhow::Result<Decimal> {
    let row: (Option<Decimal>,) = sqlx::query_as(
        "SELECT SUM(pnl_amount - fees) FROM trades \
         WHERE account_id = $1 AND status = 'closed' AND exit_at >= $2",
    )
    .bind(account_id)
    .bind(since)
    .fetch_one(pool)
    .await?;
    Ok(row.0.unwrap_or(Decimal::ZERO))
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeEventKind {
    Open,
    OvernightFee,
    SfdFee,
    SwapFee,
    Close,
}

#[derive(Debug, serde::Serialize)]
pub struct TradeEvent {
    pub kind: TradeEventKind,
    pub occurred_at: DateTime<Utc>,
    pub price: Option<Decimal>,
    pub quantity: Option<Decimal>,
    pub direction: Option<String>,
    pub cash_delta: Option<Decimal>,
    pub pnl_amount: Option<Decimal>,
}

pub async fn get_trade_events(
    pool: &PgPool,
    trade_id: Uuid,
) -> anyhow::Result<Option<Vec<TradeEvent>>> {
    let Some(trade) = get_trade_by_id(pool, trade_id).await? else {
        return Ok(None);
    };

    #[derive(sqlx::FromRow)]
    struct EventRow {
        event_type: String,
        amount: Decimal,
        occurred_at: DateTime<Utc>,
    }
    let event_rows: Vec<EventRow> = sqlx::query_as(
        r#"SELECT event_type, amount, occurred_at
           FROM account_events
           WHERE trade_id = $1
           ORDER BY occurred_at ASC, id ASC"#,
    )
    .bind(trade_id)
    .fetch_all(pool)
    .await?;

    let direction = trade.direction.as_str();

    let margin_lock_amount = event_rows
        .iter()
        .find(|r| r.event_type == "margin_lock")
        .map(|r| r.amount);
    let margin_release_amount = event_rows
        .iter()
        .find(|r| r.event_type == "margin_release")
        .map(|r| r.amount);
    let trade_close_amount = event_rows
        .iter()
        .find(|r| r.event_type == "trade_close")
        .map(|r| r.amount);
    let realized_pnl = trade_close_amount.or(trade.pnl_amount);

    let mut events = Vec::with_capacity(event_rows.len() + 2);

    events.push(TradeEvent {
        kind: TradeEventKind::Open,
        occurred_at: trade.entry_at,
        price: Some(trade.entry_price),
        quantity: Some(trade.quantity),
        direction: Some(direction.to_string()),
        cash_delta: margin_lock_amount,
        pnl_amount: None,
    });

    for row in &event_rows {
        let kind = match row.event_type.as_str() {
            "overnight_fee" => TradeEventKind::OvernightFee,
            "sfd_fee" => TradeEventKind::SfdFee,
            "swap_fee" => TradeEventKind::SwapFee,
            _ => continue,
        };
        events.push(TradeEvent {
            kind,
            occurred_at: row.occurred_at,
            price: None,
            quantity: None,
            direction: None,
            cash_delta: Some(row.amount),
            pnl_amount: None,
        });
    }

    if let (Some(exit_at), Some(exit_price)) = (trade.exit_at, trade.exit_price) {
        let cash_delta = match (margin_release_amount, realized_pnl) {
            (Some(refund), Some(pnl)) => Some(refund + pnl),
            (Some(refund), None) => Some(refund),
            (None, Some(pnl)) => Some(pnl),
            (None, None) => None,
        };
        events.push(TradeEvent {
            kind: TradeEventKind::Close,
            occurred_at: exit_at,
            price: Some(exit_price),
            quantity: Some(trade.quantity),
            direction: Some(direction.to_string()),
            cash_delta,
            pnl_amount: realized_pnl,
        });
    }

    Ok(Some(events))
}

#[derive(Debug)]
pub struct OpenTradeWithAccount {
    pub trade: Trade,
    pub account_name: Option<String>,
    pub account_type: Option<String>,
}

pub async fn list_open_with_account_name_for_pair(
    pool: &PgPool,
    exchange: &str,
    pair: &str,
) -> anyhow::Result<Vec<OpenTradeWithAccount>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        trade: TradeRow,
        account_name: Option<String>,
        account_type: Option<String>,
    }
    let rows = sqlx::query_as::<_, Row>(
        r#"SELECT t.id, t.strategy_name, t.pair, t.exchange, t.direction, t.entry_price, t.exit_price,
                  t.stop_loss, t.take_profit, t.quantity, t.leverage, t.fees, t.account_id,
                  t.entry_at, t.exit_at, t.pnl_amount,
                  t.exit_reason, t.status, t.created_at, t.max_hold_until, t.exchange_position_id,
                  t.stop_order_id,
                  ta.name AS account_name, ta.account_type AS account_type
           FROM trades t
           LEFT JOIN trading_accounts ta ON t.account_id = ta.id
           WHERE t.status = 'open' AND t.exchange = $1 AND t.pair = $2
           ORDER BY t.entry_at DESC"#,
    )
    .bind(exchange)
    .bind(pair)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            let trade: Trade = r.trade.try_into()?;
            Ok(OpenTradeWithAccount {
                trade,
                account_name: r.account_name,
                account_type: r.account_type,
            })
        })
        .collect()
}

pub async fn list_open_with_account_name(
    pool: &PgPool,
) -> anyhow::Result<Vec<OpenTradeWithAccount>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        trade: TradeRow,
        account_name: Option<String>,
        account_type: Option<String>,
    }
    let rows = sqlx::query_as::<_, Row>(
        r#"SELECT t.id, t.strategy_name, t.pair, t.exchange, t.direction, t.entry_price, t.exit_price,
                  t.stop_loss, t.take_profit, t.quantity, t.leverage, t.fees, t.account_id,
                  t.entry_at, t.exit_at, t.pnl_amount,
                  t.exit_reason, t.status, t.created_at, t.max_hold_until, t.exchange_position_id,
                  t.stop_order_id,
                  ta.name AS account_name, ta.account_type AS account_type
           FROM trades t
           LEFT JOIN trading_accounts ta ON t.account_id = ta.id
           WHERE t.status = 'open'
           ORDER BY t.entry_at DESC"#,
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            let trade: Trade = r.trade.try_into()?;
            Ok(OpenTradeWithAccount {
                trade,
                account_name: r.account_name,
                account_type: r.account_type,
            })
        })
        .collect()
}

#[derive(sqlx::FromRow)]
struct TradeRow {
    id: Uuid,
    strategy_name: String,
    pair: String,
    exchange: String,
    direction: String,
    entry_price: Decimal,
    exit_price: Option<Decimal>,
    stop_loss: Decimal,
    take_profit: Option<Decimal>,
    quantity: Decimal,
    leverage: Decimal,
    fees: Decimal,
    account_id: Uuid,
    entry_at: DateTime<Utc>,
    exit_at: Option<DateTime<Utc>>,
    pnl_amount: Option<Decimal>,
    exit_reason: Option<String>,
    status: String,
    #[allow(dead_code)]
    created_at: DateTime<Utc>,
    max_hold_until: Option<DateTime<Utc>>,
    exchange_position_id: Option<String>,
    stop_order_id: Option<String>,
}

impl TryFrom<TradeRow> for Trade {
    type Error = anyhow::Error;

    fn try_from(r: TradeRow) -> anyhow::Result<Self> {
        let exchange: Exchange = r.exchange.parse()?;
        let direction: Direction = r.direction.parse()?;
        let status: TradeStatus = r.status.parse()?;
        let exit_reason = r
            .exit_reason
            .as_deref()
            .map(|s| s.parse::<ExitReason>())
            .transpose()?;
        Ok(Trade {
            id: r.id,
            account_id: r.account_id,
            strategy_name: r.strategy_name,
            pair: Pair::new(&r.pair),
            exchange,
            direction,
            entry_price: r.entry_price,
            exit_price: r.exit_price,
            stop_loss: r.stop_loss,
            take_profit: r.take_profit,
            quantity: r.quantity,
            leverage: r.leverage,
            fees: r.fees,
            entry_at: r.entry_at,
            exit_at: r.exit_at,
            pnl_amount: r.pnl_amount,
            exit_reason,
            status,
            max_hold_until: r.max_hold_until,
            exchange_position_id: r.exchange_position_id,
            stop_order_id: r.stop_order_id,
        })
    }
}

pub async fn list_trades(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<(Vec<Trade>, i64)> {
    let jst_offset =
        chrono::FixedOffset::east_opt(9 * 3600).expect("9-hour offset is always valid");
    let from_ts = from.map(|d| {
        d.and_hms_opt(0, 0, 0)
            .expect("midnight is always valid in fixed-offset JST")
            .and_local_timezone(jst_offset)
            .single()
            .expect("midnight in JST fixed-offset is always valid")
            .with_timezone(&Utc)
    });
    let to_ts = to.map(|d| {
        (d + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .expect("midnight is always valid in fixed-offset JST")
            .and_local_timezone(jst_offset)
            .single()
            .expect("midnight in JST fixed-offset is always valid")
            .with_timezone(&Utc)
    });

    fn apply_filters<'a>(
        qb: &mut sqlx::QueryBuilder<'a, sqlx::Postgres>,
        from_ts: Option<DateTime<Utc>>,
        to_ts: Option<DateTime<Utc>>,
    ) {
        if let Some(f) = from_ts {
            qb.push(" AND entry_at >= ").push_bind(f);
        }
        if let Some(t) = to_ts {
            qb.push(" AND entry_at < ").push_bind(t);
        }
    }

    let mut select_qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
        "SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price, \
         stop_loss, take_profit, quantity, leverage, fees, account_id, \
         entry_at, exit_at, pnl_amount, exit_reason, status, created_at, max_hold_until, \
         exchange_position_id, stop_order_id \
         FROM trades WHERE 1=1",
    );
    apply_filters(&mut select_qb, from_ts, to_ts);
    select_qb
        .push(" ORDER BY entry_at DESC, id DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    let rows: Vec<TradeRow> = select_qb
        .build_query_as::<TradeRow>()
        .fetch_all(pool)
        .await?;
    let trades: anyhow::Result<Vec<Trade>> = rows.into_iter().map(|r| r.try_into()).collect();
    let trades = trades?;

    let mut count_qb: sqlx::QueryBuilder<sqlx::Postgres> =
        sqlx::QueryBuilder::new("SELECT COUNT(*) FROM trades WHERE 1=1");
    apply_filters(&mut count_qb, from_ts, to_ts);
    let total: i64 = count_qb.build_query_scalar::<i64>().fetch_one(pool).await?;

    Ok((trades, total))
}

#[cfg(test)]
mod tests;
