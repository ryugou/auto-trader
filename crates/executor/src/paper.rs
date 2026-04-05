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
    exchange: Exchange,
    leverage: Decimal,
    paper_account_id: Option<Uuid>,
}

impl PaperTrader {
    pub fn new(
        exchange: Exchange,
        initial_balance: Decimal,
        leverage: Decimal,
        paper_account_id: Option<Uuid>,
    ) -> Self {
        Self {
            balance: Mutex::new(initial_balance),
            positions: Mutex::new(HashMap::new()),
            exchange,
            leverage,
            paper_account_id,
        }
    }

    pub fn account_id(&self) -> Option<Uuid> {
        self.paper_account_id
    }

    pub fn leverage(&self) -> Decimal {
        self.leverage
    }

    pub async fn balance(&self) -> Decimal {
        *self.balance.lock().await
    }

    pub async fn execute_with_quantity(
        &self,
        signal: &Signal,
        quantity: Decimal,
    ) -> anyhow::Result<Trade> {
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
            leverage: self.leverage,
            fees: Decimal::ZERO,
            paper_account_id: self.paper_account_id,
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
            "Paper OPEN: {} {} {:?} @ {} qty={}",
            trade.strategy_name,
            trade.pair,
            trade.direction,
            trade.entry_price,
            quantity
        );
        Ok(trade)
    }

    /// Apply overnight fee to all open positions.
    /// Returns total fees charged.
    pub async fn apply_overnight_fees(&self, fee_rate: Decimal) -> Decimal {
        let mut positions = self.positions.lock().await;
        let mut balance = self.balance.lock().await;
        let mut total_fees = Decimal::ZERO;
        for trade in positions.values_mut() {
            let notional = trade.entry_price * trade.quantity.unwrap_or(Decimal::ONE);
            let fee = notional * fee_rate;
            trade.fees += fee;
            *balance -= fee;
            total_fees += fee;
        }
        total_fees
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
            exchange: self.exchange,
            direction: signal.direction,
            entry_price: signal.entry_price,
            exit_price: None,
            stop_loss: signal.stop_loss,
            take_profit: signal.take_profit,
            quantity: None,
            leverage: self.leverage,
            fees: Decimal::ZERO,
            paper_account_id: None,
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

        let (pnl_pips, pnl_amount) = if let Some(quantity) = trade.quantity {
            // Crypto/quantity-based: pnl = price_diff * quantity
            (None, price_diff * quantity)
        } else {
            // FX legacy: pip-based calculation
            let pnl_pips = Self::price_diff_to_pips(&trade.pair, price_diff);
            (Some(pnl_pips), price_diff * self.leverage)
        };

        trade.exit_price = Some(exit_price);
        trade.exit_at = Some(Utc::now());
        trade.pnl_pips = pnl_pips;
        trade.pnl_amount = Some(pnl_amount);
        trade.exit_reason = Some(exit_reason);
        trade.status = TradeStatus::Closed;

        let mut balance = self.balance.lock().await;
        *balance += pnl_amount;

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
        let trader = PaperTrader::new(Exchange::Oanda, dec!(100000), dec!(25), None);
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
        let trader = PaperTrader::new(Exchange::Oanda, dec!(100000), dec!(25), None);
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

    #[tokio::test]
    async fn crypto_position_with_quantity() {
        let trader = PaperTrader::new(Exchange::BitflyerCfd, dec!(100000), dec!(2), Some(Uuid::new_v4()));
        let signal = Signal {
            strategy_name: "crypto_trend_v1".to_string(),
            pair: Pair::new("FX_BTC_JPY"),
            direction: Direction::Long,
            entry_price: dec!(15000000),
            stop_loss: dec!(14800000),
            take_profit: dec!(15400000),
            confidence: 0.8,
            timestamp: Utc::now(),
        };
        let trade = trader
            .execute_with_quantity(&signal, dec!(0.01))
            .await
            .unwrap();
        assert_eq!(trade.quantity, Some(dec!(0.01)));
        assert_eq!(trade.leverage, dec!(2));
        assert_eq!(trade.paper_account_id, trader.account_id());

        // Close: pnl = (15400000 - 15000000) * 0.01 = 4000 JPY
        let closed = trader
            .close_position(&trade.id.to_string(), ExitReason::TpHit, dec!(15400000))
            .await
            .unwrap();
        assert_eq!(closed.pnl_amount, Some(dec!(4000)));

        // Balance: 100000 + 4000 = 104000
        assert_eq!(trader.balance().await, dec!(104000));
    }

    #[tokio::test]
    async fn overnight_fee() {
        let trader = PaperTrader::new(Exchange::BitflyerCfd, dec!(100000), dec!(2), Some(Uuid::new_v4()));
        let signal = Signal {
            strategy_name: "crypto_trend_v1".to_string(),
            pair: Pair::new("FX_BTC_JPY"),
            direction: Direction::Long,
            entry_price: dec!(15000000),
            stop_loss: dec!(14800000),
            take_profit: dec!(15400000),
            confidence: 0.8,
            timestamp: Utc::now(),
        };
        trader
            .execute_with_quantity(&signal, dec!(0.01))
            .await
            .unwrap();

        // fee_rate = 0.04% → notional = 15000000 * 0.01 = 150000 → fee = 150000 * 0.0004 = 60
        let fees = trader.apply_overnight_fees(dec!(0.0004)).await;
        assert_eq!(fees, dec!(60));
        assert_eq!(trader.balance().await, dec!(99940)); // 100000 - 60
    }
}
