use crate::report::BacktestReport;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{
    Direction, Exchange, ExitReason, Pair, Position, Trade, TradeStatus,
};
use auto_trader_executor::position_sizer::PositionSizer;
use chrono::{DateTime, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
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
/// Sizing, PnL and exchange now mirror production:
/// - quantity is computed by the same [`PositionSizer`] the live trader uses
///   (so backtests reflect the real no-liquidation allocation cap + margin
///   buffer), not a `Decimal::ONE` placeholder;
/// - `pnl_amount = (price_diff × quantity)` truncated toward zero to whole yen,
///   matching `trader.rs` (`truncate_yen`) — the old `price_diff × leverage`
///   formula ignored quantity and produced meaningless PnL;
/// - the exchange is passed in, not hardcoded.
///
/// SPREAD / SLIPPAGE: a flat `spread_pct` is applied unfavorably to every entry
/// and exit fill (buy higher, sell lower) as a first-order transaction-cost
/// approximation. **Real bid/ask spread variation and order-book depth /
/// slippage are NOT modeled** — candles carry no book data. Results are
/// therefore optimistic relative to live fills in thin or fast markets.
struct SimTrader {
    exchange: Exchange,
    leverage: Decimal,
    balance: Decimal,
    /// Flat per-side spread as a fraction of price, applied unfavorably to
    /// every fill. See struct doc for modeling limits.
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

    /// Adjust a raw price by the flat spread, in the direction that is
    /// unfavorable to the trader for the given fill side.
    ///
    /// - Buying (opening a Long / closing a Short) fills *higher*.
    /// - Selling (opening a Short / closing a Long) fills *lower*.
    fn apply_spread(&self, price: Decimal, is_buy: bool) -> Decimal {
        if is_buy {
            price * (Decimal::ONE + self.spread_pct)
        } else {
            price * (Decimal::ONE - self.spread_pct)
        }
    }

    /// Open a position sized with the production [`PositionSizer`].
    /// Returns `None` when the sizer rejects the trade (insufficient balance /
    /// below min order size) — the caller counts this as an execution failure.
    fn open(
        &mut self,
        signal: &auto_trader_core::types::Signal,
        raw_price: Decimal,
        sizer: &PositionSizer,
        liquidation_margin_level: Decimal,
        now: DateTime<Utc>,
    ) -> Option<Trade> {
        // Entry fills unfavorably: a Long buys higher, a Short sells lower.
        let entry_price = self.apply_spread(raw_price, matches!(signal.direction, Direction::Long));

        // Production sizing: same sizer / cap / buffer as the live trader.
        let quantity = sizer.calculate_quantity(
            &signal.pair,
            self.balance,
            entry_price,
            self.leverage,
            signal.allocation_pct,
            signal.stop_loss_pct,
            liquidation_margin_level,
        )?;

        // SL/TP from the actual (spread-adjusted) fill price.
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
            .ok_or_else(|| anyhow::anyhow!("position {id} not found"))?;

        // Exit fills unfavorably: closing a Long sells lower, closing a Short
        // buys higher.
        let exit_price =
            self.apply_spread(raw_exit_price, matches!(trade.direction, Direction::Short));

        let price_diff = match trade.direction {
            Direction::Long => exit_price - trade.entry_price,
            Direction::Short => trade.entry_price - exit_price,
        };

        // Quantity-based PnL, truncated toward zero to whole yen — identical to
        // production `trader.rs` (`truncate_yen(price_diff * trade.quantity)`).
        // The previous `price_diff * leverage` formula ignored quantity and was
        // a bug; the qty-based identity is guarded by a regression test.
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

pub struct BacktestRunner {
    pool: sqlx::PgPool,
}

impl BacktestRunner {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Replay historical candles for `exchange`/`pair`/`timeframe` through a
    /// strategy, sizing every entry with the production [`PositionSizer`] and
    /// replaying the strategy's own dynamic exits (`on_open_positions`) in
    /// addition to fixed SL/TP.
    ///
    /// `spread_pct` is a flat per-side transaction-cost approximation applied
    /// unfavorably to every fill; see [`SimTrader`] for its modeling limits
    /// (no order-book depth / real spread variation).
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
        // Load candles from DB — get_candles returns DESC order, reverse for
        // chronological. Exchange is now a parameter (was hardcoded "oanda").
        let mut candles = auto_trader_db::candles::get_candles(
            &self.pool,
            exchange.as_str(),
            &pair.0,
            timeframe,
            10000,
        )
        .await?;
        candles.reverse(); // chronological order

        if candles.is_empty() {
            anyhow::bail!(
                "no candle data for {} {} {}",
                exchange.as_str(),
                pair,
                timeframe
            );
        }

        let mut trader = SimTrader::new(exchange, initial_balance, leverage, spread_pct);
        let mut trades: Vec<Trade> = Vec::new();
        let mut execution_failures: usize = 0;

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
                exchange,
                candle: candle.clone(),
                indicators,
                timestamp: candle.timestamp,
            };

            // 1) Fixed SL/TP on open positions
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

            // 2) New entry signal
            if let Some(signal) = strategy.on_price(&event).await {
                // Check 1-pair-1-position per strategy
                let open = trader.open_positions();
                let has_pos = open
                    .iter()
                    .any(|t| t.strategy_name == signal.strategy_name && t.pair == signal.pair);
                if !has_pos {
                    match trader.open(
                        &signal,
                        candle.close,
                        sizer,
                        liquidation_margin_level,
                        candle.timestamp,
                    ) {
                        Some(trade) => trades.push(trade),
                        None => execution_failures += 1,
                    }
                }
            }

            // 3) Strategy-driven dynamic exits (trailing stops, reversals, …).
            // Signature is `on_open_positions(&[Position], &PriceEvent)`.
            let open_positions: Vec<Position> = trader
                .open_positions()
                .into_iter()
                .map(|trade| Position { trade })
                .collect();
            let exit_signals = strategy.on_open_positions(&open_positions, &event).await;
            for exit in exit_signals {
                let closed = trader.close(
                    exit.trade_id,
                    exit.reason.to_exit_reason(),
                    exit.close_price,
                    candle.timestamp,
                )?;
                trades.push(closed);
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
