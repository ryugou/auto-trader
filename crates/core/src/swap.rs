use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

use crate::config::SwapRateEntry;
use crate::types::Direction;

const GMO_FX_LOT_SIZE: Decimal = dec!(10_000);

pub fn compute_daily_swap(rate: SwapRateEntry, direction: Direction, quantity: Decimal) -> Decimal {
    let per_lot = match direction {
        Direction::Long => rate.long,
        Direction::Short => rate.short,
    };
    let lots = quantity / GMO_FX_LOT_SIZE;
    (per_lot * lots).round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

pub fn is_swap_rates_stale(updated_on: NaiveDate, today: NaiveDate, max_age_days: u32) -> bool {
    (today - updated_on).num_days() > i64::from(max_age_days)
}

#[cfg(test)]
mod tests;
