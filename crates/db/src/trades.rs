//! Trade DB access for the unified Trader.
//!
//! All functions operate against the `trades` schema defined in
//! `migrations/20260415000001_unified_rewrite.sql` — `account_id` rather
//! than the legacy `paper_account_id`, `quantity` is NOT NULL, and there
//! is no `mode` / `child_order_*` / `pnl_pips` column.

use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Trade, TradeStatus};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// New API for Trader
// ---------------------------------------------------------------------------

/// Insert a trade row inside the given transaction.
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
                pnl_amount, exit_reason, status, max_hold_until)
           VALUES ($1, $2, $3, $4, $5, $6,
                   $7, $8, $9, $10,
                   $11, $12, $13, $14, $15,
                   $16, $17, $18, $19)"#,
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
    .execute(executor)
    .await?;
    Ok(())
}

/// Lock margin for an open trade.
///
/// Deducts `margin_amount` from `trading_accounts.current_balance` and
/// inserts an `account_events` row with `event_type='margin_lock'`.
pub async fn lock_margin(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    margin_amount: Decimal,
) -> anyhow::Result<()> {
    // Update balance and capture new value in a single statement
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
    // Cash delta is negative — locking margin removes free cash from the
    // account. Storing the sign correctly keeps SUM(amount) reconcilable
    // against current_balance for ledger invariants.
    .bind(-margin_amount)
    .bind(new_balance)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Fetch an open trade for closing.
///
/// Must be called inside a transaction when used for concurrent-safe close
/// operations (the caller is responsible for `WHERE status = 'open'` CAS via
/// `update_trade_closed`).  Accepts any sqlx executor so callers can pass
/// either a `&PgPool` (for simple reads) or `&mut Transaction` (for locked
/// reads).
///
/// Returns `None` when the trade is not found, already closed, or belongs
/// to a different account.
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
                  exit_reason, status, created_at, max_hold_until
           FROM trades
           WHERE id = $1 AND account_id = $2 AND status = 'open'"#,
    )
    .bind(trade_id)
    .bind(account_id)
    .fetch_optional(executor)
    .await?;
    row.map(|r| r.try_into()).transpose()
}

/// Stale `closing` lock recovery threshold (seconds).
///
/// A trade left in `status='closing'` longer than this is considered
/// orphaned (e.g. process crashed mid-close, future cancelled, etc.)
/// and `acquire_close_lock` is allowed to re-acquire it. Five minutes
/// is generous: a healthy close path completes in ~1 s (single
/// `send_child_order` + `get_executions` poll within 5 s timeout).
///
/// PR-2's reconciler will provide a more aggressive sweep for these,
/// but this self-healing path ensures a single crashed Trader cannot
/// permanently strand a trade.
pub const STALE_CLOSING_THRESHOLD_SECS: i64 = 300;

/// Acquire close ownership for a trade by atomically transitioning
/// `status` from `open` to `closing`.
///
/// Returns the locked `Trade` on success. Returns `None` when the
/// trade is not in `open` status (already closed by another concurrent
/// path, or never existed). The caller MUST proceed to either
/// `update_trade_closed` (success) or `release_close_lock` (rollback).
///
/// **Cancellation safety**: if the original lock holder's future is
/// dropped (cancellation, panic, process crash) without calling
/// `release_close_lock`, the trade would normally remain stuck in
/// `closing` forever. To prevent this, the WHERE clause also accepts
/// rows already in `closing` status whose `closing_started_at` is older
/// than `STALE_CLOSING_THRESHOLD_SECS` ago — adopting the orphan
/// instead of leaking it.
///
/// This is the **only** correct entry point for closing a trade in
/// live mode: it prevents two concurrent close paths from both
/// dispatching opposite-side orders to the exchange.
pub async fn acquire_close_lock<'e, E>(
    executor: E,
    trade_id: Uuid,
    account_id: Uuid,
) -> anyhow::Result<Option<Trade>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row = sqlx::query_as::<_, TradeRow>(
        r#"UPDATE trades
           SET status = 'closing',
               closing_started_at = NOW()
           WHERE id = $1
             AND account_id = $2
             AND (
               status = 'open'
               OR (
                 status = 'closing'
                 AND closing_started_at IS NOT NULL
                 AND closing_started_at < NOW() - INTERVAL '1 second' * $3
               )
             )
           RETURNING id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                     stop_loss, take_profit, quantity, leverage, fees, account_id,
                     entry_at, exit_at, pnl_amount,
                     exit_reason, status, created_at, max_hold_until"#,
    )
    .bind(trade_id)
    .bind(account_id)
    .bind(STALE_CLOSING_THRESHOLD_SECS)
    .fetch_optional(executor)
    .await?;
    row.map(|r| r.try_into()).transpose()
}

/// Release a close lock by transitioning `closing` back to `open`.
///
/// Called when the exchange API call failed and we must NOT close the
/// trade. The status returns to `open` so subsequent close attempts can
/// retry.
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

/// Apply an overnight fee for a single trade inside a transaction.
///
/// Atomically:
///   1. Verifies the trade is still `status='open'` and belongs to the
///      account (CAS via `WHERE id=$1 AND account_id=$2 AND status='open'`).
///      Returns `Ok(None)` and skips all side effects if the trade has
///      closed or transitioned to `closing` between the caller's open-list
///      fetch and this transaction (preventing fee on an already-closed
///      trade).
///   2. Deducts `fee_amount` from `trading_accounts.current_balance`
///   3. Increments `trades.fees` for the given trade
///   4. Inserts an `account_events` row with `event_type = 'overnight_fee'`
///
/// Returns `Ok(Some(new_balance))` when the fee was applied, `Ok(None)`
/// when the trade was no longer open and nothing was changed.
pub async fn apply_overnight_fee(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
) -> anyhow::Result<Option<Decimal>> {
    // 1. Increment fees with CAS — bails out cleanly if the trade is no
    //    longer open. The UPDATE is the lock; PostgreSQL takes the row
    //    lock implicitly during the modify, so a concurrent close that
    //    also writes this row will serialise behind us (and lose the CAS
    //    if we go first, or vice versa).
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
        // Trade is closed/closing or not on this account → skip.
        return Ok(None);
    }

    // 2. Deduct from balance (SELECT … FOR UPDATE implicitly via UPDATE)
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

    // 3. Record in account_events (amount is negative to indicate outflow)
    sqlx::query(
        r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
           VALUES ($1, $2, 'overnight_fee', $3, $4)"#,
    )
    .bind(account_id)
    .bind(trade_id)
    .bind(-fee_amount)
    .bind(new_balance)
    .execute(&mut *tx)
    .await?;

    Ok(Some(new_balance))
}

/// Update a trade to closed state inside the given transaction.
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
        anyhow::bail!("trade {trade_id} not found or already closed");
    }
    Ok(())
}

/// Release margin back to the account and record pnl.
///
/// Adds `margin_return + pnl_amount` to `trading_accounts.current_balance`
/// and inserts `account_events` rows for `margin_release` and `trade_close`.
pub async fn release_margin(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    margin_return: Decimal,
    pnl_amount: Decimal,
) -> anyhow::Result<()> {
    // Return margin + pnl to balance, capture new value
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

    // Record margin_release event
    sqlx::query(
        r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after)
           VALUES ($1, $2, 'margin_release', $3, $4)"#,
    )
    .bind(account_id)
    .bind(trade_id)
    .bind(margin_return)
    .bind(new_balance - pnl_amount) // balance after margin return, before pnl credit
    .execute(&mut *tx)
    .await?;

    // Record trade_close event with pnl
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

/// Fetch all open trades for a given account.
pub async fn get_open_trades_by_account(
    pool: &PgPool,
    account_id: Uuid,
) -> anyhow::Result<Vec<Trade>> {
    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, account_id,
                  entry_at, exit_at, pnl_amount,
                  exit_reason, status, created_at, max_hold_until
           FROM trades
           WHERE account_id = $1 AND status = 'open'
           ORDER BY entry_at DESC"#,
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|r| r.try_into()).collect()
}

/// Fetch open trades for a (strategy, pair) pair. Used by the strategy engine.
pub async fn get_open_trades(
    pool: &PgPool,
    strategy_name: &str,
    pair: &str,
) -> anyhow::Result<Vec<Trade>> {
    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, account_id,
                  entry_at, exit_at, pnl_amount,
                  exit_reason, status, created_at, max_hold_until
           FROM trades
           WHERE strategy_name = $1 AND pair = $2 AND status = 'open'"#,
    )
    .bind(strategy_name)
    .bind(pair)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|r| r.try_into()).collect()
}

/// Fetch a single trade by id.
pub async fn get_trade_by_id(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<Trade>> {
    let row = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, account_id,
                  entry_at, exit_at, pnl_amount,
                  exit_reason, status, created_at, max_hold_until
           FROM trades
           WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(|r| r.try_into()).transpose()
}

/// Fetch a single open trade by id, joined with account_name and account_type.
///
/// Returns `None` when the trade is not found or is already closed.
/// Used by the exit executor to avoid a separate `get_account()` call.
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
                  t.exit_reason, t.status, t.created_at, t.max_hold_until,
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

/// Add a fee delta to trades.fees (positive delta increases fees).
pub async fn add_fees(pool: &PgPool, id: Uuid, fee_delta: Decimal) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE trades SET fees = fees + $2 WHERE id = $1")
        .bind(id)
        .bind(fee_delta)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("trade {id} not found when adding fees");
    }
    Ok(())
}

/// Discriminator for `TradeEvent` rows.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeEventKind {
    Open,
    OvernightFee,
    Close,
}

/// One entry in a single trade's chronological event timeline.
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

/// Build the event timeline for a single trade.
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

    let direction = match trade.direction {
        Direction::Long => "long",
        Direction::Short => "short",
    };

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
        if row.event_type == "overnight_fee" {
            events.push(TradeEvent {
                kind: TradeEventKind::OvernightFee,
                occurred_at: row.occurred_at,
                price: None,
                quantity: None,
                direction: None,
                cash_delta: Some(row.amount),
                pnl_amount: None,
            });
        }
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

/// Response row for positions API.
#[derive(Debug)]
pub struct OpenTradeWithAccount {
    pub trade: Trade,
    pub account_name: Option<String>,
    pub account_type: Option<String>,
}

/// Fetch open trades for a single (exchange, pair) joined with account name and type.
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
                  t.exit_reason, t.status, t.created_at, t.max_hold_until,
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

/// Fetch all currently open trades joined with account name and type.
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
                  t.exit_reason, t.status, t.created_at, t.max_hold_until,
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

// ---------------------------------------------------------------------------
// Internal row mapper
// ---------------------------------------------------------------------------

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
}

impl TryFrom<TradeRow> for Trade {
    type Error = anyhow::Error;

    fn try_from(r: TradeRow) -> anyhow::Result<Self> {
        let exchange = match r.exchange.as_str() {
            "oanda" => Exchange::Oanda,
            "bitflyer_cfd" => Exchange::BitflyerCfd,
            other => anyhow::bail!("unknown exchange: {other}"),
        };
        let direction = match r.direction.as_str() {
            "long" => Direction::Long,
            "short" => Direction::Short,
            other => anyhow::bail!("unknown direction: {other}"),
        };
        let status = match r.status.as_str() {
            "open" => TradeStatus::Open,
            "closing" => TradeStatus::Closing,
            "closed" => TradeStatus::Closed,
            other => anyhow::bail!("unknown status: {other}"),
        };
        let exit_reason = r
            .exit_reason
            .as_deref()
            .map(|s| match s {
                "tp_hit" => Ok(ExitReason::TpHit),
                "sl_hit" => Ok(ExitReason::SlHit),
                "manual" => Ok(ExitReason::Manual),
                "signal_reverse" => Ok(ExitReason::SignalReverse),
                "strategy_mean_reached" => Ok(ExitReason::StrategyMeanReached),
                "strategy_trailing_channel" => Ok(ExitReason::StrategyTrailingChannel),
                "strategy_trailing_ma" => Ok(ExitReason::StrategyTrailingMa),
                "strategy_indicator_reversal" => Ok(ExitReason::StrategyIndicatorReversal),
                "strategy_time_limit" => Ok(ExitReason::StrategyTimeLimit),
                other => Err(anyhow::anyhow!("unknown exit_reason: {other}")),
            })
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
        })
    }
}

/// Paginated trade list (for API use).
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
         entry_at, exit_at, pnl_amount, exit_reason, status, created_at, max_hold_until \
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
