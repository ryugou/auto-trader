use rust_decimal::{Decimal, RoundingStrategy};

use crate::types::Direction;

/// Open position with the prices needed to compute its margin figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenPosition {
    /// Direction of the position.
    pub direction: Direction,
    /// Price the position was opened at.
    pub entry_price: Decimal,
    /// Latest price of the pair.
    pub current_price: Decimal,
    /// Quantity of the position.
    pub quantity: Decimal,
    /// Leverage of the position.
    pub leverage: Decimal,
}

impl OpenPosition {
    /// Returns the unrealized profit or loss: the price difference in the direction of the position times `quantity`.
    pub fn unrealized_pnl(&self) -> Decimal {
        let diff = match self.direction {
            Direction::Long => self.current_price - self.entry_price,
            Direction::Short => self.entry_price - self.current_price,
        };
        diff * self.quantity
    }

    /// Returns `entry_price` times `quantity` divided by `leverage`, rounded toward zero to a whole number.
    ///
    /// # Panics
    ///
    /// Panics if `leverage` is zero.
    pub fn required_margin(&self) -> Decimal {
        (self.entry_price * self.quantity / self.leverage)
            .round_dp_with_strategy(0, RoundingStrategy::ToZero)
    }
}

/// Returns the equity divided by the total required margin of `positions`, or `None` when the total required margin is zero.
///
/// The equity is `current_balance` plus the total required margin plus the total unrealized profit or loss.
///
/// # Panics
///
/// Panics if the leverage of a position is zero.
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
