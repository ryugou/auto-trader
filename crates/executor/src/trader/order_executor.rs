mod close_position;
mod execute;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::sfd;
use auto_trader_core::types::{Direction, ExitReason, Position, Signal, Trade, TradeStatus};
use chrono::Utc;
use rust_decimal::{Decimal, RoundingStrategy};
use uuid::Uuid;

use super::Trader;

fn truncate_yen(amount: Decimal) -> Decimal {
    amount.round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

impl OrderExecutor for Trader {
    async fn execute(&self, signal: &Signal) -> anyhow::Result<Trade> {
        let (leverage, quantity, position_cutoff) = self.prepare_entry(signal).await?;

        let (fill_price, actual_qty, open_commission) = self.fill_open(signal, quantity).await?;

        let (stop_loss, take_profit) = Self::compute_stop_levels(signal, fill_price);

        let entry_at = Utc::now();
        let margin = truncate_yen(fill_price * actual_qty / leverage);

        let exchange_position_id = self
            .resolve_exchange_position_id(signal, position_cutoff, actual_qty)
            .await;

        let stop_order_id = self
            .place_protective_stop_order(
                signal,
                actual_qty,
                stop_loss,
                exchange_position_id.as_deref(),
            )
            .await;

        let trade = Trade {
            id: Uuid::new_v4(),
            account_id: self.account_id,
            strategy_name: signal.strategy_name.clone(),
            pair: signal.pair.clone(),
            exchange: self.exchange,
            direction: signal.direction,
            entry_price: fill_price,
            exit_price: None,
            stop_loss,
            take_profit,
            quantity: actual_qty,
            leverage,
            fees: open_commission,
            entry_at,
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: signal.max_hold_until,
            exchange_position_id,
            stop_order_id,
        };

        self.persist_open_trade(&trade, margin, fill_price).await?;

        self.notify_order_filled(&trade, actual_qty, fill_price, entry_at);

        tracing::info!(
            strategy_name = %trade.strategy_name,
            pair = %trade.pair,
            direction = ?trade.direction,
            fill_price = %fill_price,
            quantity = %actual_qty,
            margin_locked = %margin,
            dry_run = self.dry_run,
            "opened trade"
        );

        Ok(trade)
    }

    async fn close_position(&self, id: &str, exit_reason: ExitReason) -> anyhow::Result<Trade> {
        let (uuid, trade, was_stale_recovery) = self.load_trade_for_close(id).await?;

        let live_sfd_snapshot: Option<Decimal> = if self.dry_run {
            None
        } else {
            Some(self.snapshot_live_sfd_before_close(&trade).await)
        };

        let (exit_price, close_commission, stale_approximate) = self
            .close_and_release_lock_on_error(uuid, &trade, was_stale_recovery)
            .await?;

        let sfd_accrued = live_sfd_snapshot
            .unwrap_or_else(|| sfd::estimate(self.exchange, exit_price, trade.quantity));

        let price_diff = match trade.direction {
            Direction::Long => exit_price - trade.entry_price,
            Direction::Short => trade.entry_price - exit_price,
        };
        let pnl_amount = truncate_yen(price_diff * trade.quantity);
        let exit_at = Utc::now();

        let closed_trade = Trade {
            id: trade.id,
            account_id: trade.account_id,
            strategy_name: trade.strategy_name.clone(),
            pair: trade.pair.clone(),
            exchange: trade.exchange,
            direction: trade.direction,
            entry_price: trade.entry_price,
            exit_price: Some(exit_price),
            stop_loss: trade.stop_loss,
            take_profit: trade.take_profit,
            quantity: trade.quantity,
            leverage: trade.leverage,
            fees: trade.fees + close_commission + sfd_accrued,
            entry_at: trade.entry_at,
            exit_at: Some(exit_at),
            pnl_amount: Some(pnl_amount),
            exit_reason: Some(exit_reason),
            status: TradeStatus::Closed,
            max_hold_until: trade.max_hold_until,
            exchange_position_id: trade.exchange_position_id.clone(),
            stop_order_id: trade.stop_order_id.clone(),
        };

        let margin = truncate_yen(trade.entry_price * trade.quantity / trade.leverage);

        self.persist_closed_trade(
            &closed_trade,
            margin,
            pnl_amount,
            exit_price,
            exit_at,
            exit_reason,
        )
        .await?;

        self.notify_close_result(&closed_trade, pnl_amount, exit_reason, stale_approximate);

        tracing::info!(
            strategy_name = %closed_trade.strategy_name,
            pair = %closed_trade.pair,
            pnl_amount = %pnl_amount,
            exit_reason = ?exit_reason,
            dry_run = self.dry_run,
            "closed trade"
        );

        Ok(closed_trade)
    }

    async fn open_positions(&self) -> anyhow::Result<Vec<Position>> {
        let trades =
            auto_trader_db::trades::get_open_trades_by_account(&self.pool, self.account_id).await?;
        Ok(trades.into_iter().map(|trade| Position { trade }).collect())
    }
}
