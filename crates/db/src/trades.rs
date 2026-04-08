use auto_trader_core::types::{
    Direction, Exchange, ExitReason, Pair, Trade, TradeMode, TradeStatus,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn insert_trade(pool: &PgPool, trade: &Trade) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    insert_trade_with_executor(&mut *conn, trade).await
}

/// Insert a trade row using a caller-provided executor (a transaction
/// or a connection). Used by `PaperTrader::execute_with_quantity` to
/// keep the trade INSERT atomic with the balance update + event row,
/// without duplicating the column / serialization logic in the
/// executor crate.
pub async fn insert_trade_with_executor<'e, E>(
    executor: E,
    trade: &Trade,
) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        r#"INSERT INTO trades
           (id, strategy_name, pair, exchange, direction, entry_price, exit_price,
            stop_loss, take_profit, quantity, leverage, fees, paper_account_id,
            entry_at, exit_at, pnl_pips, pnl_amount,
            exit_reason, mode, status, max_hold_until)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)"#,
    )
    .bind(trade.id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(trade.exchange.as_str())
    .bind(serde_json::to_string(&trade.direction)?.trim_matches('"').to_string())
    .bind(trade.entry_price)
    .bind(trade.exit_price)
    .bind(trade.stop_loss)
    .bind(trade.take_profit)
    .bind(trade.quantity)
    .bind(trade.leverage)
    .bind(trade.fees)
    .bind(trade.paper_account_id)
    .bind(trade.entry_at)
    .bind(trade.exit_at)
    .bind(trade.pnl_pips)
    .bind(trade.pnl_amount)
    .bind(trade.exit_reason.map(|r| {
        serde_json::to_string(&r)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string()
    }))
    .bind(serde_json::to_string(&trade.mode).unwrap_or_default().trim_matches('"').to_string())
    .bind(serde_json::to_string(&trade.status).unwrap_or_default().trim_matches('"').to_string())
    .bind(trade.max_hold_until)
    .execute(executor)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn update_trade_closed(
    pool: &PgPool,
    id: Uuid,
    exit_price: Decimal,
    exit_at: DateTime<Utc>,
    pnl_pips: Option<Decimal>,
    pnl_amount: Decimal,
    exit_reason: ExitReason,
    fees: Decimal,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"UPDATE trades
           SET exit_price = $2, exit_at = $3, pnl_pips = $4, pnl_amount = $5,
               exit_reason = $6, fees = $7, status = 'closed'
           WHERE id = $1"#,
    )
    .bind(id)
    .bind(exit_price)
    .bind(exit_at)
    .bind(pnl_pips)
    .bind(pnl_amount)
    .bind(serde_json::to_string(&exit_reason).unwrap_or_default().trim_matches('"'))
    .bind(fees)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_open_trades(
    pool: &PgPool,
    strategy_name: &str,
    pair: &str,
) -> anyhow::Result<Vec<Trade>> {
    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, paper_account_id,
                  entry_at, exit_at, pnl_pips, pnl_amount,
                  exit_reason, mode, status, created_at, max_hold_until
           FROM trades
           WHERE strategy_name = $1 AND pair = $2 AND status = 'open'"#,
    )
    .bind(strategy_name)
    .bind(pair)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|r| r.try_into()).collect()
}

/// Fetch all open trades for a given paper account.
pub async fn get_open_trades_by_account(
    pool: &PgPool,
    paper_account_id: Uuid,
) -> anyhow::Result<Vec<Trade>> {
    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, paper_account_id,
                  entry_at, exit_at, pnl_pips, pnl_amount,
                  exit_reason, mode, status, created_at, max_hold_until
           FROM trades
           WHERE paper_account_id = $1 AND status = 'open'
           ORDER BY entry_at ASC"#,
    )
    .bind(paper_account_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|r| r.try_into()).collect()
}

/// Fetch a single trade by id.
pub async fn get_trade_by_id(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<Trade>> {
    let row = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
                  stop_loss, take_profit, quantity, leverage, fees, paper_account_id,
                  entry_at, exit_at, pnl_pips, pnl_amount,
                  exit_reason, mode, status, created_at, max_hold_until
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

/// Response row for positions API — joins with paper_accounts to include account name.
#[derive(Debug)]
pub struct OpenTradeWithAccount {
    pub trade: Trade,
    pub paper_account_name: Option<String>,
}

/// Fetch open trades for a single (exchange, pair) joined with the paper
/// account name. Used by the strategy engine on every price tick — we
/// scope to the event's exchange/pair in SQL so the query stays cheap as
/// the open-trade table grows, instead of pulling every open trade and
/// filtering in Rust.
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
                  t.stop_loss, t.take_profit, t.quantity, t.leverage, t.fees, t.paper_account_id,
                  t.entry_at, t.exit_at, t.pnl_pips, t.pnl_amount,
                  t.exit_reason, t.mode, t.status, t.created_at, t.max_hold_until,
                  pa.name AS account_name
           FROM trades t
           LEFT JOIN paper_accounts pa ON t.paper_account_id = pa.id
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

/// Fetch all currently open trades joined with the paper account name.
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
                  t.stop_loss, t.take_profit, t.quantity, t.leverage, t.fees, t.paper_account_id,
                  t.entry_at, t.exit_at, t.pnl_pips, t.pnl_amount,
                  t.exit_reason, t.mode, t.status, t.created_at, t.max_hold_until,
                  pa.name AS account_name
           FROM trades t
           LEFT JOIN paper_accounts pa ON t.paper_account_id = pa.id
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
    take_profit: Decimal,
    quantity: Option<Decimal>,
    leverage: Decimal,
    fees: Decimal,
    paper_account_id: Option<Uuid>,
    entry_at: DateTime<Utc>,
    exit_at: Option<DateTime<Utc>>,
    pnl_pips: Option<Decimal>,
    pnl_amount: Option<Decimal>,
    exit_reason: Option<String>,
    mode: String,
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
        let mode = match r.mode.as_str() {
            "live" => TradeMode::Live,
            "paper" => TradeMode::Paper,
            "backtest" => TradeMode::Backtest,
            other => anyhow::bail!("unknown mode: {other}"),
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
            paper_account_id: r.paper_account_id,
            entry_at: r.entry_at,
            exit_at: r.exit_at,
            pnl_pips: r.pnl_pips,
            pnl_amount: r.pnl_amount,
            exit_reason,
            mode,
            status,
            max_hold_until: r.max_hold_until,
        })
    }
}
