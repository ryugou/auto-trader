use auto_trader_core::types::Trade;

/// Inserts `trade` with all its fields into the `trades` table.
///
/// # Errors
///
/// - Returns an error if the database query fails.
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
