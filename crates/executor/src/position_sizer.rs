use std::collections::HashMap;

use auto_trader_core::types::{Direction, Pair};
use rust_decimal::Decimal;

pub struct PositionSizer {
    min_order_sizes: HashMap<Pair, Decimal>,
    margin_buffer: Decimal,
    price_units: HashMap<Pair, Decimal>,
}

impl PositionSizer {
    pub fn new(min_order_sizes: HashMap<Pair, Decimal>, margin_buffer: Decimal) -> Self {
        Self {
            min_order_sizes,
            margin_buffer,
            price_units: HashMap::new(),
        }
    }

    pub fn with_price_units(mut self, price_units: HashMap<Pair, Decimal>) -> Self {
        self.price_units = price_units;
        self
    }

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
