use std::collections::HashMap;

use auto_trader_core::types::{Direction, Pair};
use rust_decimal::Decimal;

/// Sizer that calculates order quantities and rounds trigger prices per currency pair.
pub struct PositionSizer {
    min_order_sizes: HashMap<Pair, Decimal>,
    margin_buffer: Decimal,
    price_units: HashMap<Pair, Decimal>,
}

impl PositionSizer {
    /// Creates a sizer from the minimum order size of each pair and the margin buffer added to the liquidation margin level.
    pub fn new(min_order_sizes: HashMap<Pair, Decimal>, margin_buffer: Decimal) -> Self {
        Self {
            min_order_sizes,
            margin_buffer,
            price_units: HashMap::new(),
        }
    }

    /// Sets the price unit of each pair that trigger prices are rounded to.
    pub fn with_price_units(mut self, price_units: HashMap<Pair, Decimal>) -> Self {
        self.price_units = price_units;
        self
    }

    /// Rounds a trigger price to a multiple of the pair's price unit: up for a long, down for a short.
    ///
    /// Returns the price unchanged when the pair has no positive price unit.
    pub fn round_trigger_price(
        &self,
        pair: &Pair,
        price: Decimal,
        direction: Direction,
    ) -> Decimal {
        let unit = self.price_units.get(pair).copied().unwrap_or(Decimal::ZERO);
        if unit <= Decimal::ZERO {
            return price;
        }
        let steps = price / unit;
        let rounded = match direction {
            Direction::Long => steps.ceil(),
            Direction::Short => steps.floor(),
        };
        rounded * unit
    }

    /// Calculates the order quantity, truncated to a multiple of the pair's minimum order size when the pair has a positive one.
    ///
    /// Returns `None` when `balance`, `entry_price`, `leverage`, `stop_loss_pct` or `liquidation_margin_level` is not positive, when `allocation_pct` is not in `(0, 1]`, or when the quantity is below the minimum order size.
    #[allow(clippy::too_many_arguments)]
    pub fn calculate_quantity(
        &self,
        pair: &Pair,
        balance: Decimal,
        entry_price: Decimal,
        leverage: Decimal,
        allocation_pct: Decimal,
        stop_loss_pct: Decimal,
        liquidation_margin_level: Decimal,
    ) -> Option<Decimal> {
        if balance <= Decimal::ZERO
            || entry_price <= Decimal::ZERO
            || leverage <= Decimal::ZERO
            || allocation_pct <= Decimal::ZERO
            || allocation_pct > Decimal::ONE
            || stop_loss_pct <= Decimal::ZERO
            || liquidation_margin_level <= Decimal::ZERO
        {
            return None;
        }

        let y_eff = liquidation_margin_level + self.margin_buffer;
        let max_alloc = Decimal::ONE / (y_eff + leverage * stop_loss_pct);
        let risk_alloc = max_alloc.min(allocation_pct);

        let raw_qty = balance * leverage * risk_alloc / entry_price;

        let min_size = self
            .min_order_sizes
            .get(pair)
            .copied()
            .unwrap_or(Decimal::ZERO);

        if min_size > Decimal::ZERO {
            let truncated = (raw_qty / min_size).floor() * min_size;
            if truncated < min_size {
                return None;
            }
            Some(truncated)
        } else if raw_qty > Decimal::ZERO {
            Some(raw_qty)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests;
