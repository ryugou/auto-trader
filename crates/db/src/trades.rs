use auto_trader_core::types::{
    Direction, ExitReason, Pair, Trade, TradeMode, TradeStatus,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn insert_trade(pool: &PgPool, trade: &Trade) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO trades
           (id, strategy_name, pair, direction, entry_price, exit_price,
            stop_loss, take_profit, entry_at, exit_at, pnl_pips, pnl_amount,
            exit_reason, mode, status)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)"#,
    )
    .bind(trade.id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(serde_json::to_string(&trade.direction)?.trim_matches('"'))
    .bind(trade.entry_price)
    .bind(trade.exit_price)
    .bind(trade.stop_loss)
    .bind(trade.take_profit)
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
    .bind(serde_json::to_string(&trade.mode).unwrap_or_default().trim_matches('"'))
    .bind(serde_json::to_string(&trade.status).unwrap_or_default().trim_matches('"'))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_trade_closed(
    pool: &PgPool,
    id: Uuid,
    exit_price: Decimal,
    exit_at: DateTime<Utc>,
    pnl_pips: Decimal,
    pnl_amount: Decimal,
    exit_reason: ExitReason,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"UPDATE trades
           SET exit_price = $2, exit_at = $3, pnl_pips = $4, pnl_amount = $5,
               exit_reason = $6, status = 'closed'
           WHERE id = $1"#,
    )
    .bind(id)
    .bind(exit_price)
    .bind(exit_at)
    .bind(pnl_pips)
    .bind(pnl_amount)
    .bind(serde_json::to_string(&exit_reason).unwrap_or_default().trim_matches('"'))
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
        r#"SELECT id, strategy_name, pair, direction, entry_price, exit_price,
                  stop_loss, take_profit, entry_at, exit_at, pnl_pips, pnl_amount,
                  exit_reason, mode, status, created_at
           FROM trades
           WHERE strategy_name = $1 AND pair = $2 AND status = 'open'"#,
    )
    .bind(strategy_name)
    .bind(pair)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|r| r.try_into()).collect()
}

#[derive(sqlx::FromRow)]
struct TradeRow {
    id: Uuid,
    strategy_name: String,
    pair: String,
    direction: String,
    entry_price: Decimal,
    exit_price: Option<Decimal>,
    stop_loss: Decimal,
    take_profit: Decimal,
    entry_at: DateTime<Utc>,
    exit_at: Option<DateTime<Utc>>,
    pnl_pips: Option<Decimal>,
    pnl_amount: Option<Decimal>,
    exit_reason: Option<String>,
    mode: String,
    status: String,
    created_at: DateTime<Utc>,
}

impl TryFrom<TradeRow> for Trade {
    type Error = anyhow::Error;

    fn try_from(r: TradeRow) -> anyhow::Result<Self> {
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
                other => Err(anyhow::anyhow!("unknown exit_reason: {other}")),
            })
            .transpose()?;
        Ok(Trade {
            id: r.id,
            strategy_name: r.strategy_name,
            pair: Pair::new(&r.pair),
            direction,
            entry_price: r.entry_price,
            exit_price: r.exit_price,
            stop_loss: r.stop_loss,
            take_profit: r.take_profit,
            entry_at: r.entry_at,
            exit_at: r.exit_at,
            pnl_pips: r.pnl_pips,
            pnl_amount: r.pnl_amount,
            exit_reason,
            mode,
            status,
        })
    }
}
