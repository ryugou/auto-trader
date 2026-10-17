use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::config::PairConfig;
use auto_trader_core::types::Pair;
use auto_trader_executor::position_sizer::PositionSizer;
use rust_decimal::Decimal;

pub(crate) fn build(
    pair_configs: &HashMap<String, PairConfig>,
    sizing_margin_buffer: Decimal,
) -> Arc<PositionSizer> {
    let min_order_sizes = collect_pair_decimals(pair_configs, |v| v.min_order_size);
    let price_units = collect_pair_decimals(pair_configs, |v| v.price_unit);
    Arc::new(
        PositionSizer::new(min_order_sizes, sizing_margin_buffer).with_price_units(price_units),
    )
}

fn collect_pair_decimals(
    pair_configs: &HashMap<String, PairConfig>,
    pick: impl Fn(&PairConfig) -> Decimal,
) -> HashMap<Pair, Decimal> {
    pair_configs
        .iter()
        .map(|(k, v)| (Pair::new(k), pick(v)))
        .collect()
}

#[cfg(test)]
mod tests;
