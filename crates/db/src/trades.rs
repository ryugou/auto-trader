//! Trade DB access.
//!
//! NOTE: This module is partially stubbed. Core functions (`insert_trade`,
//! `lock_margin`, `release_margin`, `get_trade_for_close`,
//! `update_trade_closed`) return `unimplemented!()` pending the full
//! schema migration in PR-1 Task 6.
//!
//! Legacy query functions (`get_open_trades`, `get_trade_events`, etc.)
//! are preserved but may fail at runtime against the new schema.
//! They will be updated in Task 6.

use auto_trader_core::types::{
    Direction, Exchange, ExitReason, Pair, Trade, TradeStatus,
};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// New API for Trader (Task 6 will provide real implementations)
// ---------------------------------------------------------------------------

/// Insert a trade row inside the given transaction.
///
/// # Panics (temporary)
///
/// This is a stub. The real implementation (aligned with the new schema) is
/// delivered in PR-1 Task 6.
pub async fn insert_trade<'e, E>(_executor: E, _trade: &Trade) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    unimplemented!("implemented in PR-1 Task 6 — new trades schema not yet migrated");
}

/// Lock margin for an open trade.
///
/// Deducts `margin_amount` from `trading_accounts.current_balance` and
/// inserts an `account_events` row with `event_type='margin_lock'`.
///
/// # Panics (temporary)
pub async fn lock_margin<'e, E>(
    _executor: E,
    _account_id: Uuid,
    _trade_id: Uuid,
    _margin_amount: Decimal,
) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    unimplemented!("implemented in PR-1 Task 6 — account_events schema not yet migrated");
}

/// Fetch a trade for closing (SELECT … FOR UPDATE).
///
/// Returns `None` when the trade is not found, already closed, or belongs
/// to a different account.
///
/// # Panics (temporary)
pub async fn get_trade_for_close(
    _pool: &PgPool,
    _trade_id: Uuid,
    _account_id: Uuid,
) -> anyhow::Result<Option<Trade>> {
    unimplemented!("implemented in PR-1 Task 6 — new trades schema not yet migrated");
}

/// Update a trade to closed state inside the given transaction.
///
/// # Panics (temporary)
pub async fn update_trade_closed<'e, E>(
    _executor: E,
    _trade_id: Uuid,
    _exit_price: Decimal,
    _exit_at: DateTime<Utc>,
    _pnl_amount: Decimal,
    _exit_reason: ExitReason,
    _fees: Decimal,
) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    unimplemented!("implemented in PR-1 Task 6 — new trades schema not yet migrated");
}

/// Release margin back to the account and record pnl.
///
/// Adds `margin_return + pnl_amount` to `trading_accounts.current_balance`
/// and inserts `account_events` rows for `margin_release` and `trade_close`.
///
/// # Panics (temporary)
pub async fn release_margin<'e, E>(
    _executor: E,
    _account_id: Uuid,
    _trade_id: Uuid,
    _margin_return: Decimal,
    _pnl_amount: Decimal,
) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    unimplemented!("implemented in PR-1 Task 6 — account_events schema not yet migrated");
}

/// Fetch all open trades for a given account.
///
/// # Panics (temporary)
pub async fn get_open_trades_by_account(
    _pool: &PgPool,
    _account_id: Uuid,
) -> anyhow::Result<Vec<Trade>> {
    unimplemented!("implemented in PR-1 Task 6 — new trades schema not yet migrated");
}

// ---------------------------------------------------------------------------
// Legacy query helpers — preserved for other callers, updated in Task 6
// ---------------------------------------------------------------------------

// TODO(PR-1 Task 6): Remove or rewrite all functions below. They reference
// the old schema (paper_account_id, pnl_pips, mode, child_order_*) and will
// fail against the new `trades` table.

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
    pub paper_account_name: Option<String>,
}

/// Fetch open trades for a single (exchange, pair) joined with account name.
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
    }
    let rows = sqlx::query_as::<_, Row>(
        r#"SELECT t.id, t.strategy_name, t.pair, t.exchange, t.direction, t.entry_price, t.exit_price,
                  t.stop_loss, t.take_profit, t.quantity, t.leverage, t.fees, t.account_id,
                  t.entry_at, t.exit_at, t.pnl_amount,
                  t.exit_reason, t.status, t.created_at, t.max_hold_until,
                  ta.name AS account_name
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
                paper_account_name: r.account_name,
            })
        })
        .collect()
}

/// Fetch all currently open trades joined with account name.
pub async fn list_open_with_account_name(
    pool: &PgPool,
) -> anyhow::Result<Vec<OpenTradeWithAccount>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        trade: TradeRow,
        account_name: Option<String>,
    }
    let rows = sqlx::query_as::<_, Row>(
        r#"SELECT t.id, t.strategy_name, t.pair, t.exchange, t.direction, t.entry_price, t.exit_price,
                  t.stop_loss, t.take_profit, t.quantity, t.leverage, t.fees, t.account_id,
                  t.entry_at, t.exit_at, t.pnl_amount,
                  t.exit_reason, t.status, t.created_at, t.max_hold_until,
                  ta.name AS account_name
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
                paper_account_name: r.account_name,
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
            .unwrap()
            .and_local_timezone(jst_offset)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    });
    let to_ts = to.map(|d| {
        (d + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(jst_offset)
            .single()
            .unwrap()
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
