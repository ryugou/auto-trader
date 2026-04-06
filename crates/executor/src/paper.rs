//! DB-backed paper trader.
//!
//! All state (balance, open positions) lives in the database.
//! PaperTrader holds no in-memory state beyond its pool/exchange/account_id.
//! This ensures that restarts do not lose any trades or balance information.

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::*;
use chrono::Utc;
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

pub struct PaperTrader {
    pool: PgPool,
    exchange: Exchange,
    paper_account_id: Uuid,
}

impl PaperTrader {
    pub fn new(pool: PgPool, exchange: Exchange, paper_account_id: Uuid) -> Self {
        Self {
            pool,
            exchange,
            paper_account_id,
        }
    }

    pub fn account_id(&self) -> Uuid {
        self.paper_account_id
    }

    pub fn exchange(&self) -> Exchange {
        self.exchange
    }

    /// Fetch current balance from DB.
    pub async fn balance(&self) -> anyhow::Result<Decimal> {
        auto_trader_db::paper_accounts::get_paper_account(&self.pool, self.paper_account_id)
            .await?
            .map(|a| a.current_balance)
            .ok_or_else(|| anyhow::anyhow!("paper account {} not found", self.paper_account_id))
    }

    /// Fetch leverage from DB.
    pub async fn leverage(&self) -> anyhow::Result<Decimal> {
        auto_trader_db::paper_accounts::get_paper_account(&self.pool, self.paper_account_id)
            .await?
            .map(|a| a.leverage)
            .ok_or_else(|| anyhow::anyhow!("paper account {} not found", self.paper_account_id))
    }

    /// Open a position with an explicit quantity (crypto path).
    pub async fn execute_with_quantity(
        &self,
        signal: &Signal,
        quantity: Decimal,
    ) -> anyhow::Result<Trade> {
        let leverage = self.leverage().await?;
        let trade = Trade {
            id: Uuid::new_v4(),
            strategy_name: signal.strategy_name.clone(),
            pair: signal.pair.clone(),
            exchange: self.exchange,
            direction: signal.direction,
            entry_price: signal.entry_price,
            exit_price: None,
            stop_loss: signal.stop_loss,
            take_profit: signal.take_profit,
            quantity: Some(quantity),
            leverage,
            fees: Decimal::ZERO,
            paper_account_id: Some(self.paper_account_id),
            entry_at: Utc::now(),
            exit_at: None,
            pnl_pips: None,
            pnl_amount: None,
            exit_reason: None,
            mode: TradeMode::Paper,
            status: TradeStatus::Open,
        };
        auto_trader_db::trades::insert_trade(&self.pool, &trade).await?;
        tracing::info!(
            "Paper OPEN: {} {} {:?} @ {} qty={}",
            trade.strategy_name,
            trade.pair,
            trade.direction,
            trade.entry_price,
            quantity
        );
        Ok(trade)
    }

    /// Apply overnight fee to all open positions for this account.
    /// Returns total fees charged. Updates trades.fees and paper_accounts.current_balance in DB.
    pub async fn apply_overnight_fees(&self, fee_rate: Decimal) -> anyhow::Result<Decimal> {
        let positions = self.open_trades_internal().await?;
        let mut total_fees = Decimal::ZERO;
        for trade in positions {
            let quantity = trade.quantity.unwrap_or(Decimal::ONE);
            let notional = trade.entry_price * quantity;
            let fee = notional * fee_rate;
            if fee == Decimal::ZERO {
                continue;
            }
            auto_trader_db::trades::add_fees(&self.pool, trade.id, fee).await?;
            total_fees += fee;
        }
        if total_fees > Decimal::ZERO {
            auto_trader_db::paper_accounts::add_pnl(
                &self.pool,
                self.paper_account_id,
                -total_fees,
            )
            .await?;
        }
        Ok(total_fees)
    }

    async fn open_trades_internal(&self) -> anyhow::Result<Vec<Trade>> {
        auto_trader_db::trades::get_open_trades_by_account(&self.pool, self.paper_account_id).await
    }

    fn calculate_price_diff(direction: Direction, entry: Decimal, exit: Decimal) -> Decimal {
        match direction {
            Direction::Long => exit - entry,
            Direction::Short => entry - exit,
        }
    }

    /// Convert price difference to pips based on pair convention.
    /// JPY pairs: 1 pip = 0.01, others: 1 pip = 0.0001
    fn price_diff_to_pips(pair: &Pair, price_diff: Decimal) -> Decimal {
        let pip_size = if pair.0.contains("JPY") {
            Decimal::new(1, 2) // 0.01
        } else {
            Decimal::new(1, 4) // 0.0001
        };
        price_diff / pip_size
    }
}

impl OrderExecutor for PaperTrader {
    async fn execute(&self, signal: &Signal) -> anyhow::Result<Trade> {
        // FX-style open (no explicit quantity).
        let leverage = self.leverage().await?;
        let trade = Trade {
            id: Uuid::new_v4(),
            strategy_name: signal.strategy_name.clone(),
            pair: signal.pair.clone(),
            exchange: self.exchange,
            direction: signal.direction,
            entry_price: signal.entry_price,
            exit_price: None,
            stop_loss: signal.stop_loss,
            take_profit: signal.take_profit,
            quantity: None,
            leverage,
            fees: Decimal::ZERO,
            paper_account_id: Some(self.paper_account_id),
            entry_at: Utc::now(),
            exit_at: None,
            pnl_pips: None,
            pnl_amount: None,
            exit_reason: None,
            mode: TradeMode::Paper,
            status: TradeStatus::Open,
        };
        auto_trader_db::trades::insert_trade(&self.pool, &trade).await?;
        tracing::info!(
            "Paper OPEN: {} {} {:?} @ {}",
            trade.strategy_name,
            trade.pair,
            trade.direction,
            trade.entry_price
        );
        Ok(trade)
    }

    async fn open_positions(&self) -> anyhow::Result<Vec<Position>> {
        let trades = self.open_trades_internal().await?;
        Ok(trades.into_iter().map(|trade| Position { trade }).collect())
    }

    async fn close_position(
        &self,
        id: &str,
        exit_reason: ExitReason,
        exit_price: Decimal,
    ) -> anyhow::Result<Trade> {
        let uuid = Uuid::parse_str(id)?;
        let mut trade = auto_trader_db::trades::get_trade_by_id(&self.pool, uuid)
            .await?
            .ok_or_else(|| anyhow::anyhow!("trade {id} not found"))?;

        if trade.status == TradeStatus::Closed {
            anyhow::bail!("trade {id} already closed");
        }

        let price_diff = Self::calculate_price_diff(trade.direction, trade.entry_price, exit_price);
        let leverage = trade.leverage;
        let (pnl_pips, pnl_amount) = if let Some(quantity) = trade.quantity {
            // Crypto/quantity-based: pnl = price_diff * quantity
            (None, price_diff * quantity)
        } else {
            // FX: pip-based calculation
            let pnl_pips = Self::price_diff_to_pips(&trade.pair, price_diff);
            (Some(pnl_pips), price_diff * leverage)
        };

        let exit_at = Utc::now();
        trade.exit_price = Some(exit_price);
        trade.exit_at = Some(exit_at);
        trade.pnl_pips = pnl_pips;
        trade.pnl_amount = Some(pnl_amount);
        trade.exit_reason = Some(exit_reason);
        trade.status = TradeStatus::Closed;

        // Persist trade close.
        auto_trader_db::trades::update_trade_closed(
            &self.pool,
            trade.id,
            exit_price,
            exit_at,
            pnl_pips,
            pnl_amount,
            exit_reason,
            trade.fees,
        )
        .await?;

        // Persist balance delta. Fees are deducted from the balance when they
        // are charged (e.g. overnight fees), so we only add the gross pnl here
        // to avoid double-counting.
        auto_trader_db::paper_accounts::add_pnl(
            &self.pool,
            self.paper_account_id,
            pnl_amount,
        )
        .await?;

        tracing::info!(
            "Paper CLOSE: {} {} pnl={} reason={:?}",
            trade.strategy_name,
            trade.pair,
            pnl_amount,
            exit_reason
        );
        Ok(trade)
    }
}

#[cfg(test)]
mod tests {
    //! DB-dependent tests are intentionally omitted.
    //! Only pure functions are unit-tested here. Integration tests with a real
    //! Postgres instance can be added under crates/app/tests if needed.
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn calculate_price_diff_long() {
        assert_eq!(
            PaperTrader::calculate_price_diff(Direction::Long, dec!(150), dec!(151)),
            dec!(1)
        );
    }

    #[test]
    fn calculate_price_diff_short() {
        assert_eq!(
            PaperTrader::calculate_price_diff(Direction::Short, dec!(150), dec!(149)),
            dec!(1)
        );
    }

    #[test]
    fn price_diff_to_pips_jpy_pair() {
        // USD_JPY: 1.00 price diff = 100 pips
        assert_eq!(
            PaperTrader::price_diff_to_pips(&Pair::new("USD_JPY"), dec!(1.00)),
            dec!(100)
        );
    }

    #[test]
    fn price_diff_to_pips_non_jpy_pair() {
        // EUR_USD: 0.0050 price diff = 50 pips
        assert_eq!(
            PaperTrader::price_diff_to_pips(&Pair::new("EUR_USD"), dec!(0.0050)),
            dec!(50)
        );
    }
}
