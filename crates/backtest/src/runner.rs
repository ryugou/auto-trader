use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{Direction, ExitReason, Pair, Trade};
use auto_trader_executor::paper::PaperTrader;
use auto_trader_core::executor::OrderExecutor;
use crate::report::BacktestReport;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;

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
        let mut candles = auto_trader_db::candles::get_candles(
            &self.pool, &pair.0, timeframe, 10000
        ).await?;
        candles.reverse(); // chronological order

        if candles.is_empty() {
            anyhow::bail!("no candle data for {} {}", pair, timeframe);
        }

        let trader = Arc::new(PaperTrader::new(initial_balance, leverage));
        let mut trades: Vec<Trade> = Vec::new();

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
                candle: candle.clone(),
                indicators,
                timestamp: candle.timestamp,
            };

            // Check SL/TP on open positions
            let positions = trader.open_positions().await?;
            for pos in positions {
                let t = &pos.trade;
                if t.pair != *pair { continue; }
                let exit = match t.direction {
                    Direction::Long => {
                        if candle.low <= t.stop_loss { Some((ExitReason::SlHit, t.stop_loss)) }
                        else if candle.high >= t.take_profit { Some((ExitReason::TpHit, t.take_profit)) }
                        else { None }
                    }
                    Direction::Short => {
                        if candle.high >= t.stop_loss { Some((ExitReason::SlHit, t.stop_loss)) }
                        else if candle.low <= t.take_profit { Some((ExitReason::TpHit, t.take_profit)) }
                        else { None }
                    }
                };
                if let Some((reason, price)) = exit {
                    let closed = trader.close_position(&t.id.to_string(), reason, price).await?;
                    trades.push(closed);
                }
            }

            // Run strategy
            if let Some(signal) = strategy.on_price(&event).await {
                // Check 1-pair-1-position per strategy
                let open = trader.open_positions().await?;
                let has_pos = open.iter().any(|p| {
                    p.trade.strategy_name == signal.strategy_name && p.trade.pair == signal.pair
                });
                if !has_pos {
                    if let Ok(trade) = trader.execute(&signal).await {
                        trades.push(trade);
                    }
                }
            }
        }

        let final_balance = trader.balance().await;
        Ok(BacktestReport::from_trades(trades, initial_balance, final_balance))
    }
}
