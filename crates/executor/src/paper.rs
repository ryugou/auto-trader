use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::*;
use chrono::Utc;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::Mutex;
use uuid::Uuid;

pub struct PaperTrader {
    balance: Mutex<Decimal>,
    positions: Mutex<HashMap<Uuid, Trade>>,
    leverage: Decimal,
}

impl PaperTrader {
    pub fn new(initial_balance: Decimal, leverage: Decimal) -> Self {
        Self {
            balance: Mutex::new(initial_balance),
            positions: Mutex::new(HashMap::new()),
            leverage,
        }
    }

    pub async fn balance(&self) -> Decimal {
        *self.balance.lock().await
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
        let trade = Trade {
            id: Uuid::new_v4(),
            strategy_name: signal.strategy_name.clone(),
            pair: signal.pair.clone(),
            direction: signal.direction,
            entry_price: signal.entry_price,
            exit_price: None,
            stop_loss: signal.stop_loss,
            take_profit: signal.take_profit,
            entry_at: Utc::now(),
            exit_at: None,
            pnl_pips: None,
            pnl_amount: None,
            exit_reason: None,
            mode: TradeMode::Paper,
            status: TradeStatus::Open,
        };
        self.positions.lock().await.insert(trade.id, trade.clone());
        tracing::info!(
            "Paper OPEN: {} {} {} @ {}",
            trade.strategy_name,
            trade.pair,
            serde_json::to_string(&trade.direction).unwrap_or_default(),
            trade.entry_price
        );
        Ok(trade)
    }

    async fn open_positions(&self) -> anyhow::Result<Vec<Position>> {
        let positions = self.positions.lock().await;
        Ok(positions
            .values()
            .map(|t| Position { trade: t.clone() })
            .collect())
    }

    async fn close_position(
        &self,
        id: &str,
        exit_reason: ExitReason,
        exit_price: Decimal,
    ) -> anyhow::Result<Trade> {
        let uuid = Uuid::parse_str(id)?;
        let mut positions = self.positions.lock().await;
        let mut trade = positions
            .remove(&uuid)
            .ok_or_else(|| anyhow::anyhow!("position {id} not found"))?;

        let price_diff = Self::calculate_price_diff(trade.direction, trade.entry_price, exit_price);
        let pnl_pips = Self::price_diff_to_pips(&trade.pair, price_diff);
        trade.exit_price = Some(exit_price);
        trade.exit_at = Some(Utc::now());
        trade.pnl_pips = Some(pnl_pips);
        trade.pnl_amount = Some(price_diff * self.leverage);
        trade.exit_reason = Some(exit_reason);
        trade.status = TradeStatus::Closed;

        let mut balance = self.balance.lock().await;
        *balance += trade.pnl_amount.unwrap_or_default();

        tracing::info!(
            "Paper CLOSE: {} {} pnl={} pips reason={:?}",
            trade.strategy_name,
            trade.pair,
            pnl_pips,
            exit_reason
        );
        Ok(trade)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_signal() -> Signal {
        Signal {
            strategy_name: "test_strat".to_string(),
            pair: Pair::new("USD_JPY"),
            direction: Direction::Long,
            entry_price: dec!(150.00),
            stop_loss: dec!(149.50),
            take_profit: dec!(151.00),
            confidence: 0.8,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn open_and_close_position() {
        let trader = PaperTrader::new(dec!(100000), dec!(25));
        let trade = trader.execute(&test_signal()).await.unwrap();
        assert_eq!(trade.status, TradeStatus::Open);
        assert_eq!(trade.mode, TradeMode::Paper);

        let positions = trader.open_positions().await.unwrap();
        assert_eq!(positions.len(), 1);

        // USD_JPY: 1 pip = 0.01, so 151.00 - 150.00 = 1.00 = 100 pips
        let closed = trader
            .close_position(&trade.id.to_string(), ExitReason::TpHit, dec!(151.00))
            .await
            .unwrap();
        assert_eq!(closed.status, TradeStatus::Closed);
        assert_eq!(closed.pnl_pips, Some(dec!(100))); // 1.00 / 0.01 = 100 pips
        assert_eq!(closed.pnl_amount, Some(dec!(25.00))); // price_diff * leverage = 1.00 * 25

        let positions = trader.open_positions().await.unwrap();
        assert_eq!(positions.len(), 0);

        assert_eq!(trader.balance().await, dec!(100025));
    }

    #[tokio::test]
    async fn short_position_pnl() {
        let trader = PaperTrader::new(dec!(100000), dec!(25));
        let mut signal = test_signal();
        signal.direction = Direction::Short;
        let trade = trader.execute(&signal).await.unwrap();

        // USD_JPY: short from 150.00, exit 150.50 = -0.50 price diff = -50 pips
        let closed = trader
            .close_position(&trade.id.to_string(), ExitReason::SlHit, dec!(150.50))
            .await
            .unwrap();
        assert_eq!(closed.pnl_pips, Some(dec!(-50)));
    }

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
