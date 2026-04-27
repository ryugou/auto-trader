use crate::event::PriceEvent;
use crate::types::{Direction, ExitReason, Position, Signal};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Clone)]
pub struct MacroUpdate {
    pub summary: String,
    pub adjustments: std::collections::HashMap<String, String>,
}

/// Reason a strategy is asking to close an open position. Stored on the
/// trade row so we can attribute exits to the strategy logic that fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyExitReason {
    /// Mean-reversion target (e.g. price returned to BB middle / SMA20).
    MeanReached,
    /// Trailing channel break (e.g. Donchian opposite-side break).
    TrailingChannel,
    /// Trailing moving average break (e.g. close below EMA21).
    TrailingMa,
    /// Indicator-based reversal (e.g. RSI crossed back through midline).
    IndicatorReversal,
    /// Time-based fail-safe — strategy gave up waiting.
    TimeLimit,
    /// Catch-all for strategy-defined custom reasons.
    Custom(&'static str),
}

impl StrategyExitReason {
    /// String tag persisted to `trades.exit_reason`. Stable across versions.
    pub fn as_tag(&self) -> &'static str {
        match self {
            Self::MeanReached => "strategy_mean_reached",
            Self::TrailingChannel => "strategy_trailing_channel",
            Self::TrailingMa => "strategy_trailing_ma",
            Self::IndicatorReversal => "strategy_indicator_reversal",
            Self::TimeLimit => "strategy_time_limit",
            Self::Custom(tag) => tag,
        }
    }

    /// Map a strategy-specific exit reason onto the canonical `ExitReason`
    /// enum stored on the trade row. `Custom` falls back to `Manual`
    /// because we don't add a fresh enum variant per ad-hoc string.
    pub fn to_exit_reason(&self) -> ExitReason {
        match self {
            Self::MeanReached => ExitReason::StrategyMeanReached,
            Self::TrailingChannel => ExitReason::StrategyTrailingChannel,
            Self::TrailingMa => ExitReason::StrategyTrailingMa,
            Self::IndicatorReversal => ExitReason::StrategyIndicatorReversal,
            Self::TimeLimit => ExitReason::StrategyTimeLimit,
            Self::Custom(_) => ExitReason::Manual,
        }
    }
}

/// A strategy's request to close one of its open positions. Emitted from
/// `Strategy::on_open_positions` and consumed by the executor.
///
/// `close_price` is the price at which the strategy wants to mark the
/// position out. Strategies typically pass `event.candle.close` from the
/// PriceEvent they were called with — this gives the executor a real
/// price instead of a fallback so the recorded P&L is accurate.
#[derive(Debug, Clone)]
pub struct ExitSignal {
    pub trade_id: Uuid,
    pub reason: StrategyExitReason,
    pub close_price: rust_decimal::Decimal,
}

/// Check if unrealized profit has reached 1R (= initial SL distance).
/// Used by strategy exit logic to prevent exiting before the trade has
/// moved at least as far as the stop-loss in the profit direction.
///
/// Defensive: if SL or entry is invalid (legacy row, DB corruption),
/// returns `true` to allow exits to proceed — don't block normal exit
/// logic due to bad data.
pub fn has_reached_one_r(
    direction: &Direction,
    entry_price: Decimal,
    stop_loss: Decimal,
    current_price: Decimal,
) -> bool {
    // Defensive: if SL or entry is invalid (legacy row, DB corruption),
    // don't block exits — return true to let normal exit logic proceed.
    if entry_price <= Decimal::ZERO || stop_loss <= Decimal::ZERO {
        return true;
    }
    let sl_distance = (entry_price - stop_loss).abs();
    if sl_distance.is_zero() {
        return true; // SL at entry = no meaningful 1R threshold.
    }
    let unrealized = match direction {
        Direction::Long => current_price - entry_price,
        Direction::Short => entry_price - current_price,
    };
    unrealized >= sl_distance
}

#[async_trait::async_trait]
pub trait Strategy: Send + 'static {
    fn name(&self) -> &str;
    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal>;
    fn on_macro_update(&mut self, update: &MacroUpdate);

    /// Seed internal state from historical PriceEvents (oldest → newest).
    /// Called once at startup before any live event so the strategy can build
    /// up indicator history from DB instead of waiting for real-time candles.
    /// Implementations must NOT emit signals here.
    async fn warmup(&mut self, _events: &[PriceEvent]) {}

    /// Inspect the strategy's currently-open positions on each price event
    /// and decide whether any of them should be closed via strategy-driven
    /// logic (trailing stops, indicator reversals, time limits, …).
    ///
    /// The fixed `stop_loss` / `take_profit` levels recorded on the trade
    /// row are still enforced independently by the position monitor — this
    /// callback adds *additional* dynamic exit conditions on top of those.
    ///
    /// Default impl returns no exits, which preserves the previous
    /// "fixed SL/TP only" behavior for strategies that don't opt in.
    async fn on_open_positions(
        &mut self,
        _positions: &[Position],
        _event: &PriceEvent,
    ) -> Vec<ExitSignal> {
        Vec::new()
    }
}
