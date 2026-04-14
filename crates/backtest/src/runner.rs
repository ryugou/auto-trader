use crate::report::BacktestReport;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Trade, TradeStatus};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

/// Sentinel UUID used for SimTrader's in-memory backtest trades.
/// Backtest trades are never persisted to DB, so account_id is a placeholder.
const BACKTEST_ACCOUNT_ID: Uuid = Uuid::nil();

/// In-memory simulated trader used for backtests only.
/// This is deliberately separate from the production `Trader`, which is
/// DB-backed. Backtests run fully in-memory on historical candles and do not
/// (and should not) touch persistent storage.
///
/// NOTE: Currently supports **FX backtests only**. SimTrader has no explicit
/// `quantity` and no overnight-fee model, so crypto strategies that rely on
/// position sizing or swap fees will not produce accurate results. Crypto
/// backtest support is deferred to a future iteration.
struct SimTrader {
    exchange: Exchange,
    leverage: Decimal,
    balance: Decimal,
    positions: HashMap<Uuid, Trade>,
}

impl SimTrader {
    fn new(exchange: Exchange, initial_balance: Decimal, leverage: Decimal) -> Self {
        Self {
            exchange,
            leverage,
            balance: initial_balance,
            positions: HashMap::new(),
        }
    }

    fn open(
        &mut self,
        signal: &auto_trader_core::types::Signal,
        entry_price: Decimal,
        now: DateTime<Utc>,
    ) -> Trade {
        // Compute SL/TP from the actual candle close price passed by the caller.
        let stop_loss = match signal.direction {
            Direction::Long => entry_price * (Decimal::ONE - signal.stop_loss_pct),
            Direction::Short => entry_price * (Decimal::ONE + signal.stop_loss_pct),
        };
        let take_profit = signal.take_profit_pct.map(|pct| match signal.direction {
            Direction::Long => entry_price * (Decimal::ONE + pct),
            Direction::Short => entry_price * (Decimal::ONE - pct),
        });
        let trade = Trade {
            id: Uuid::new_v4(),
            account_id: BACKTEST_ACCOUNT_ID,
            strategy_name: signal.strategy_name.clone(),
            pair: signal.pair.clone(),
            exchange: self.exchange,
            direction: signal.direction,
            entry_price,
            exit_price: None,
            stop_loss,
            take_profit,
            quantity: Decimal::ONE, // placeholder — backtest doesn't size
            leverage: self.leverage,
            fees: Decimal::ZERO,
            entry_at: now,
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: signal.max_hold_until,
        };
        self.positions.insert(trade.id, trade.clone());
        trade
    }

    fn open_positions(&self) -> Vec<Trade> {
        self.positions.values().cloned().collect()
    }

    fn close(
        &mut self,
        id: Uuid,
        reason: ExitReason,
        exit_price: Decimal,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Trade> {
        let mut trade = self
            .positions
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("position {id} not found"))?;

        let price_diff = match trade.direction {
            Direction::Long => exit_price - trade.entry_price,
            Direction::Short => trade.entry_price - exit_price,
        };

        let pnl_amount = price_diff * self.leverage;

        trade.exit_price = Some(exit_price);
        trade.exit_at = Some(now);
        trade.pnl_amount = Some(pnl_amount);
        trade.exit_reason = Some(reason);
        trade.status = TradeStatus::Closed;

        self.balance += pnl_amount;
        Ok(trade)
    }

    fn balance(&self) -> Decimal {
        self.balance
    }
}

pub struct BacktestRunner {
    pool: sqlx::PgPool,
}

impl BacktestRunner {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    pub async fn run(
        &self,
        strategy: &mut dyn Strategy,
        pair: &Pair,
        timeframe: &str,
        initial_balance: Decimal,
        leverage: Decimal,
    ) -> anyhow::Result<BacktestReport> {
        // Load candles from DB — get_candles returns DESC order, reverse for chronological
        let mut candles =
            auto_trader_db::candles::get_candles(&self.pool, "oanda", &pair.0, timeframe, 10000)
                .await?;
        candles.reverse(); // chronological order

        if candles.is_empty() {
            anyhow::bail!("no candle data for {} {}", pair, timeframe);
        }

        let mut trader = SimTrader::new(Exchange::Oanda, initial_balance, leverage);
        let mut trades: Vec<Trade> = Vec::new();
        let execution_failures: usize = 0;

        // Replay candles chronologically
        for (i, candle) in candles.iter().enumerate() {
            // Build indicators from available history
            let closes: Vec<Decimal> = candles[..=i].iter().map(|c| c.close).collect();
            let mut indicators = HashMap::new();
            if let Some(v) = auto_trader_market::indicators::sma(&closes, 20) {
                indicators.insert("sma_20".to_string(), v);
            }
            if let Some(v) = auto_trader_market::indicators::sma(&closes, 50) {
                indicators.insert("sma_50".to_string(), v);
            }
            if let Some(v) = auto_trader_market::indicators::rsi(&closes, 14) {
                indicators.insert("rsi_14".to_string(), v);
            }

            let event = PriceEvent {
                pair: pair.clone(),
                exchange: Exchange::Oanda,
                candle: candle.clone(),
                indicators,
                timestamp: candle.timestamp,
            };

            // Check SL/TP on open positions
            let open = trader.open_positions();
            for t in open {
                if t.pair != *pair {
                    continue;
                }
                let exit = match t.direction {
                    Direction::Long => {
                        if candle.low <= t.stop_loss {
                            Some((ExitReason::SlHit, t.stop_loss))
                        } else if t.take_profit.is_some_and(|tp| candle.high >= tp) {
                            Some((ExitReason::TpHit, t.take_profit.expect("checked above")))
                        } else {
                            None
                        }
                    }
                    Direction::Short => {
                        if candle.high >= t.stop_loss {
                            Some((ExitReason::SlHit, t.stop_loss))
                        } else if t.take_profit.is_some_and(|tp| candle.low <= tp) {
                            Some((ExitReason::TpHit, t.take_profit.expect("checked above")))
                        } else {
                            None
                        }
                    }
                };
                if let Some((reason, price)) = exit {
                    let closed = trader.close(t.id, reason, price, candle.timestamp)?;
                    trades.push(closed);
                }
            }

            // Run strategy
            if let Some(signal) = strategy.on_price(&event).await {
                // Check 1-pair-1-position per strategy
                let open = trader.open_positions();
                let has_pos = open
                    .iter()
                    .any(|t| t.strategy_name == signal.strategy_name && t.pair == signal.pair);
                if !has_pos {
                    let trade = trader.open(&signal, candle.close, candle.timestamp);
                    trades.push(trade);
                }
            }
        }

        let final_balance = trader.balance();
        Ok(BacktestReport::from_trades_with_failures(
            trades,
            initial_balance,
            final_balance,
            execution_failures,
        ))
    }
}
