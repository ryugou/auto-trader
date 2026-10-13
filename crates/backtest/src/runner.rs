use std::collections::HashMap;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{
    Candle, Direction, Exchange, ExitReason, Pair, Position, Trade, TradeStatus,
};
use auto_trader_executor::position_sizer::PositionSizer;
use chrono::{DateTime, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use uuid::Uuid;

use crate::report::BacktestReport;

const BACKTEST_ACCOUNT_ID: Uuid = Uuid::nil();

struct SimTrader {
    exchange: Exchange,
    leverage: Decimal,
    balance: Decimal,
    spread_pct: Decimal,
    positions: HashMap<Uuid, Trade>,
}

impl SimTrader {
    fn new(
        exchange: Exchange,
        initial_balance: Decimal,
        leverage: Decimal,
        spread_pct: Decimal,
    ) -> Self {
        Self {
            exchange,
            leverage,
            balance: initial_balance,
            spread_pct,
            positions: HashMap::new(),
        }
    }

    fn apply_spread(&self, price: Decimal, is_buy: bool) -> Decimal {
        if is_buy {
            price * (Decimal::ONE + self.spread_pct)
        } else {
            price * (Decimal::ONE - self.spread_pct)
        }
    }

    fn open(
        &mut self,
        signal: &auto_trader_core::types::Signal,
        raw_price: Decimal,
        sizer: &PositionSizer,
        liquidation_margin_level: Decimal,
        now: DateTime<Utc>,
    ) -> Option<Trade> {
        let entry_price = self.apply_spread(raw_price, matches!(signal.direction, Direction::Long));

        let quantity = sizer.calculate_quantity(
            &signal.pair,
            self.balance,
            entry_price,
            self.leverage,
            signal.allocation_pct,
            signal.stop_loss_pct,
            liquidation_margin_level,
        )?;

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
            quantity,
            leverage: self.leverage,
            fees: Decimal::ZERO,
            entry_at: now,
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: signal.max_hold_until,
            exchange_position_id: None,
            stop_order_id: None,
        };
        self.positions.insert(trade.id, trade.clone());
        Some(trade)
    }

    fn open_positions(&self) -> Vec<Trade> {
        self.positions.values().cloned().collect()
    }

    fn close(
        &mut self,
        id: Uuid,
        reason: ExitReason,
        raw_exit_price: Decimal,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Trade> {
        let mut trade = self
            .positions
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("invalid position \"{id}\": not found"))?;

        let exit_price =
            self.apply_spread(raw_exit_price, matches!(trade.direction, Direction::Short));

        let price_diff = match trade.direction {
            Direction::Long => exit_price - trade.entry_price,
            Direction::Short => trade.entry_price - exit_price,
        };

        let pnl_amount =
            (price_diff * trade.quantity).round_dp_with_strategy(0, RoundingStrategy::ToZero);

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

const SMA_SHORT_PERIOD: usize = 20;
const SMA_LONG_PERIOD: usize = 50;
const RSI_PERIOD: usize = 14;
const INDICATOR_LOOKBACK_CANDLES: usize = SMA_LONG_PERIOD;

fn recent_closes(candles: &[Candle], i: usize) -> Vec<Decimal> {
    let start = (i + 1).saturating_sub(INDICATOR_LOOKBACK_CANDLES);
    candles[start..=i].iter().map(|c| c.close).collect()
}

fn indicators_up_to(candles: &[Candle], i: usize) -> HashMap<String, Decimal> {
    let closes = recent_closes(candles, i);
    let mut indicators = HashMap::new();
    if let Some(v) = auto_trader_market::indicators::sma(&closes, SMA_SHORT_PERIOD) {
        indicators.insert("sma_20".to_string(), v);
    }
    if let Some(v) = auto_trader_market::indicators::sma(&closes, SMA_LONG_PERIOD) {
        indicators.insert("sma_50".to_string(), v);
    }
    if let Some(v) = auto_trader_market::indicators::rsi(&closes, RSI_PERIOD) {
        indicators.insert("rsi_14".to_string(), v);
    }
    indicators
}

fn fixed_exit_for(t: &Trade, candle: &Candle) -> Option<(ExitReason, Decimal)> {
    match t.direction {
        Direction::Long => {
            if candle.low <= t.stop_loss {
                Some((ExitReason::SlHit, t.stop_loss))
            } else if let Some(tp) = t.take_profit
                && candle.high >= tp
            {
                Some((ExitReason::TpHit, tp))
            } else {
                None
            }
        }
        Direction::Short => {
            if candle.high >= t.stop_loss {
                Some((ExitReason::SlHit, t.stop_loss))
            } else if let Some(tp) = t.take_profit
                && candle.low <= tp
            {
                Some((ExitReason::TpHit, tp))
            } else {
                None
            }
        }
    }
}

fn close_hit_fixed_exits(
    trader: &mut SimTrader,
    pair: &Pair,
    candle: &Candle,
    trades: &mut Vec<Trade>,
) -> anyhow::Result<()> {
    for t in trader.open_positions() {
        if t.pair != *pair {
            continue;
        }
        let Some((reason, price)) = fixed_exit_for(&t, candle) else {
            continue;
        };
        let closed = trader.close(t.id, reason, price, candle.timestamp)?;
        trades.push(closed);
    }
    Ok(())
}

async fn try_open_new_entry(
    trader: &mut SimTrader,
    strategy: &mut dyn Strategy,
    event: &PriceEvent,
    sizer: &PositionSizer,
    liquidation_margin_level: Decimal,
    trades: &mut Vec<Trade>,
    execution_failures: &mut usize,
) {
    let Some(signal) = strategy.on_price(event).await else {
        return;
    };
    let has_pos = trader
        .open_positions()
        .iter()
        .any(|t| t.strategy_name == signal.strategy_name && t.pair == signal.pair);
    if has_pos {
        return;
    }
    match trader.open(
        &signal,
        event.candle.close,
        sizer,
        liquidation_margin_level,
        event.timestamp,
    ) {
        Some(trade) => trades.push(trade),
        None => *execution_failures += 1,
    }
}

async fn close_strategy_driven_exits(
    trader: &mut SimTrader,
    strategy: &mut dyn Strategy,
    event: &PriceEvent,
    trades: &mut Vec<Trade>,
) -> anyhow::Result<()> {
    let open_positions: Vec<Position> = trader
        .open_positions()
        .into_iter()
        .map(|trade| Position { trade })
        .collect();
    let exit_signals = strategy.on_open_positions(&open_positions, event).await;
    for exit in exit_signals {
        let closed = trader.close(
            exit.trade_id,
            exit.reason.to_exit_reason(),
            exit.close_price,
            event.timestamp,
        )?;
        trades.push(closed);
    }
    Ok(())
}

pub struct BacktestRunner {
    pool: sqlx::PgPool,
}

impl BacktestRunner {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    async fn load_chronological_candles(
        &self,
        exchange: Exchange,
        pair: &Pair,
        timeframe: &str,
    ) -> anyhow::Result<Vec<Candle>> {
        let mut candles = auto_trader_db::candles::get_candles(
            &self.pool,
            exchange.as_str(),
            &pair.0,
            timeframe,
            10000,
        )
        .await?;
        candles.reverse();

        if candles.is_empty() {
            anyhow::bail!(
                "failed to load candles: no data for exchange {} pair {} timeframe {}",
                exchange.as_str(),
                pair,
                timeframe
            );
        }

        Ok(candles)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &self,
        strategy: &mut dyn Strategy,
        exchange: Exchange,
        pair: &Pair,
        timeframe: &str,
        initial_balance: Decimal,
        leverage: Decimal,
        sizer: &PositionSizer,
        liquidation_margin_level: Decimal,
        spread_pct: Decimal,
    ) -> anyhow::Result<BacktestReport> {
        let candles = self
            .load_chronological_candles(exchange, pair, timeframe)
            .await?;

        let mut trader = SimTrader::new(exchange, initial_balance, leverage, spread_pct);
        let mut trades: Vec<Trade> = Vec::new();
        let mut execution_failures: usize = 0;

        for (i, candle) in candles.iter().enumerate() {
            let event = PriceEvent {
                pair: pair.clone(),
                exchange,
                candle: candle.clone(),
                indicators: indicators_up_to(&candles, i),
                timestamp: candle.timestamp,
            };

            close_hit_fixed_exits(&mut trader, pair, candle, &mut trades)?;
            try_open_new_entry(
                &mut trader,
                strategy,
                &event,
                sizer,
                liquidation_margin_level,
                &mut trades,
                &mut execution_failures,
            )
            .await;
            close_strategy_driven_exits(&mut trader, strategy, &event, &mut trades).await?;
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

#[cfg(test)]
mod tests;
