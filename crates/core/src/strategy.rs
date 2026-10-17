use rust_decimal::Decimal;
use uuid::Uuid;

use crate::event::PriceEvent;
use crate::types::{Direction, ExitReason, Position, Signal};

/// Macro analysis update that is delivered to strategies.
#[derive(Clone)]
pub struct MacroUpdate {
    /// Summary of the analysis.
    pub summary: String,
    /// Adjustments keyed by name, with their values as strings.
    pub adjustments: std::collections::HashMap<String, String>,
}

/// Reason a strategy gives for closing a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyExitReason {
    /// The price reached the mean.
    MeanReached,
    /// The price hit the trailing channel.
    TrailingChannel,
    /// The price hit the trailing moving average.
    TrailingMa,
    /// An indicator reversed.
    IndicatorReversal,
    /// The holding time limit was reached.
    TimeLimit,
    /// A reason that carries its own tag.
    Custom(&'static str),
}

impl StrategyExitReason {
    /// Returns the tag of the reason: `strategy_` followed by the snake_case name, or the custom tag as is.
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

    /// Returns the matching `ExitReason`, which is `ExitReason::Manual` for `Custom`.
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

/// Request from a strategy to close a trade.
#[derive(Debug, Clone)]
pub struct ExitSignal {
    /// Identifier of the trade to close.
    pub trade_id: Uuid,
    /// Reason for closing the trade.
    pub reason: StrategyExitReason,
    /// Price to close the trade at.
    pub close_price: rust_decimal::Decimal,
}

/// Returns whether the unrealized profit has reached the stop loss distance, which is 1R.
///
/// Also returns `true` when `entry_price` or `stop_loss` is not positive, or when `stop_loss` is not below `entry_price` for a long or not above it for a short.
pub fn has_reached_one_r(
    direction: &Direction,
    entry_price: Decimal,
    stop_loss: Decimal,
    current_price: Decimal,
) -> bool {
    if entry_price <= Decimal::ZERO || stop_loss <= Decimal::ZERO {
        return true;
    }
    let sl_on_correct_side = match direction {
        Direction::Long => stop_loss < entry_price,
        Direction::Short => stop_loss > entry_price,
    };
    if !sl_on_correct_side {
        return true;
    }
    let sl_distance = (entry_price - stop_loss).abs();
    let unrealized = match direction {
        Direction::Long => current_price - entry_price,
        Direction::Short => entry_price - current_price,
    };
    unrealized >= sl_distance
}

/// Trading strategy that turns price events into signals and exit requests.
#[async_trait::async_trait]
pub trait Strategy: Send + 'static {
    /// Returns the name of the strategy.
    fn name(&self) -> &str;
    /// Handles a price event and returns a signal when the strategy decides to trade.
    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal>;
    /// Handles a macro update.
    fn on_macro_update(&mut self, update: &MacroUpdate);

    /// Feeds past price events to the strategy; does nothing by default.
    async fn warmup(&mut self, _events: &[PriceEvent]) {}

    /// Returns exit requests for the open positions at the given price event; returns none by default.
    async fn on_open_positions(
        &mut self,
        _positions: &[Position],
        _event: &PriceEvent,
    ) -> Vec<ExitSignal> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests;
