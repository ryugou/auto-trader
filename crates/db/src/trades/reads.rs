use auto_trader_core::types::Trade;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use super::row::TradeRow;
use crate::jst_day_start::{jst_day_start_utc, jst_next_day_start_utc};

/// Returns the open and closing trades of the account, newest entry first.
///
/// # Errors
///
/// - Returns an error if the database query fails.
/// - Returns an error if a stored row has an exchange, direction, status or exit reason that is not supported.
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

/// Returns the open trades of the account, newest entry first.
///
/// # Errors
///
/// - Returns an error if the database query fails.
/// - Returns an error if a stored row has an exchange, direction, status or exit reason that is not supported.
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

/// Returns the open trades of the strategy for the pair.
///
/// # Errors
///
/// - Returns an error if the database query fails.
/// - Returns an error if a stored row has an exchange, direction, status or exit reason that is not supported.
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

/// Returns the trade with the given identifier whatever its status, or `None` if there is none.
///
/// # Errors
///
/// - Returns an error if the database query fails.
/// - Returns an error if the stored row has an exchange, direction, status or exit reason that is not supported.
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

/// Open trade together with the name and type of the account that holds it.
#[derive(Debug)]
pub struct OpenTradeWithAccount {
    /// The open trade.
    pub trade: Trade,
    /// Name of the account; `None` if no account row matches.
    pub account_name: Option<String>,
    /// Type of the account; `None` if no account row matches.
    pub account_type: Option<String>,
}

#[derive(sqlx::FromRow)]
struct OpenTradeWithAccountRow {
    #[sqlx(flatten)]
    trade: TradeRow,
    account_name: Option<String>,
    account_type: Option<String>,
}

impl TryFrom<OpenTradeWithAccountRow> for OpenTradeWithAccount {
    type Error = anyhow::Error;

    fn try_from(r: OpenTradeWithAccountRow) -> anyhow::Result<Self> {
        Ok(Self {
            trade: r.trade.try_into()?,
            account_name: r.account_name,
            account_type: r.account_type,
        })
    }
}

/// Returns the open trade with the given identifier together with its account, or `None` if there is no open trade with that identifier.
///
/// # Errors
///
/// - Returns an error if the database query fails.
/// - Returns an error if the stored row has an exchange, direction, status or exit reason that is not supported.
pub async fn get_open_trade_with_account(
    pool: &PgPool,
    trade_id: Uuid,
) -> anyhow::Result<Option<OpenTradeWithAccount>> {
    let row = sqlx::query_as::<_, OpenTradeWithAccountRow>(
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
    row.map(OpenTradeWithAccount::try_from).transpose()
}

/// Returns the sum of the profit or loss minus the fees of the closed trades of the account whose exit time is at or after `since`, or zero if there are none.
///
/// # Errors
///
/// - Returns an error if the database query fails.
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

/// Kind of an event in the history of a trade.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeEventKind {
    /// The trade was opened.
    Open,
    /// An overnight fee was charged.
    OvernightFee,
    /// An SFD fee was charged.
    SfdFee,
    /// A swap fee was charged.
    SwapFee,
    /// The trade was closed.
    Close,
}

/// Event in the history of a trade, with the price, quantity and amounts that apply to it.
#[derive(Debug, serde::Serialize)]
pub struct TradeEvent {
    /// Kind of the event.
    pub kind: TradeEventKind,
    /// Time of the event.
    pub occurred_at: DateTime<Utc>,
    /// Entry price for `Open` and exit price for `Close`; `None` for a fee event.
    pub price: Option<Decimal>,
    /// Quantity of the trade for `Open` and `Close`; `None` for a fee event.
    pub quantity: Option<Decimal>,
    /// Direction of the trade for `Open` and `Close`; `None` for a fee event.
    pub direction: Option<String>,
    /// Change of the account cash caused by the event, taken from the recorded account events and, for `Close`, the profit or loss of the trade; `None` when neither is recorded.
    pub cash_delta: Option<Decimal>,
    /// Realized profit or loss for `Close`; `None` for the other kinds.
    pub pnl_amount: Option<Decimal>,
}

/// Returns the events of the trade in order: `Open`, then the fee events in the order they occurred, then `Close` if the trade has an exit time and an exit price. Returns `None` if there is no such trade.
///
/// # Errors
///
/// - Returns an error if the database query fails.
/// - Returns an error if the stored trade has an exchange, direction, status or exit reason that is not supported.
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

/// Returns the open trades on the exchange for the pair, each with its account, newest entry first.
///
/// # Errors
///
/// - Returns an error if the database query fails.
/// - Returns an error if a stored row has an exchange, direction, status or exit reason that is not supported.
pub async fn list_open_with_account_name_for_pair(
    pool: &PgPool,
    exchange: &str,
    pair: &str,
) -> anyhow::Result<Vec<OpenTradeWithAccount>> {
    let rows = sqlx::query_as::<_, OpenTradeWithAccountRow>(
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
        .map(OpenTradeWithAccount::try_from)
        .collect()
}

/// Returns all open trades, each with its account, newest entry first.
///
/// # Errors
///
/// - Returns an error if the database query fails.
/// - Returns an error if a stored row has an exchange, direction, status or exit reason that is not supported.
pub async fn list_open_with_account_name(
    pool: &PgPool,
) -> anyhow::Result<Vec<OpenTradeWithAccount>> {
    let rows = sqlx::query_as::<_, OpenTradeWithAccountRow>(
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
        .map(OpenTradeWithAccount::try_from)
        .collect()
}

/// Returns a page of trades, newest entry first, together with the number of trades that match the filters. `from` and `to` keep the trades entered from the start of the JST day of `from` up to the end of the JST day of `to`.
///
/// # Errors
///
/// - Returns an error if the JST day of `from` or `to` is out of range.
/// - Returns an error if the database query fails.
/// - Returns an error if a stored row has an exchange, direction, status or exit reason that is not supported.
pub async fn list_trades(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<(Vec<Trade>, i64)> {
    let from_ts = from.map(jst_day_start_utc).transpose()?;
    let to_ts = to.map(jst_next_day_start_utc).transpose()?;

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
