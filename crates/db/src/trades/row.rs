use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Trade, TradeStatus};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub(super) struct TradeRow {
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
