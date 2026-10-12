use rust_decimal::{Decimal, RoundingStrategy};

use crate::types::Direction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenPosition {
    pub direction: Direction,
    pub entry_price: Decimal,
    pub current_price: Decimal,
    pub quantity: Decimal,
    pub leverage: Decimal,
}

impl OpenPosition {
    pub fn unrealized_pnl(&self) -> Decimal {
        let diff = match self.direction {
            Direction::Long => self.current_price - self.entry_price,
            Direction::Short => self.entry_price - self.current_price,
        };
        diff * self.quantity
    }

    pub fn required_margin(&self) -> Decimal {
        (self.entry_price * self.quantity / self.leverage)
            .round_dp_with_strategy(0, RoundingStrategy::ToZero)
    }
}

pub fn compute_maintenance_ratio(
    current_balance: Decimal,
    positions: &[OpenPosition],
) -> Option<Decimal> {
    let total_required: Decimal = positions.iter().map(|p| p.required_margin()).sum();
    if total_required.is_zero() {
        return None;
    }
    let total_unrealized: Decimal = positions.iter().map(|p| p.unrealized_pnl()).sum();
    let equity = current_balance + total_required + total_unrealized;
    Some(equity / total_required)
}

#[cfg(test)]
mod tests;
