use rust_decimal::Decimal;
use uuid::Uuid;

use crate::event::PriceEvent;
use crate::types::{Direction, ExitReason, Position, Signal};

#[derive(Clone)]
pub struct MacroUpdate {
    pub summary: String,
    pub adjustments: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyExitReason {
    MeanReached,
    TrailingChannel,
    TrailingMa,
    IndicatorReversal,
    TimeLimit,
    Custom(&'static str),
}

impl StrategyExitReason {
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

#[derive(Debug, Clone)]
pub struct ExitSignal {
    pub trade_id: Uuid,
    pub reason: StrategyExitReason,
    pub close_price: rust_decimal::Decimal,
}

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

#[async_trait::async_trait]
pub trait Strategy: Send + 'static {
    fn name(&self) -> &str;
    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal>;
    fn on_macro_update(&mut self, update: &MacroUpdate);

    async fn warmup(&mut self, _events: &[PriceEvent]) {}

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
