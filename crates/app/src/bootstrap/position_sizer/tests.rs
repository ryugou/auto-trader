use std::collections::HashMap;

use auto_trader_core::config::PairConfig;
use auto_trader_core::types::Pair;
use rust_decimal_macros::dec;

use super::*;

fn two_pair_configs() -> HashMap<String, PairConfig> {
    let mut map = HashMap::new();
    map.insert(
        "USD_JPY".to_string(),
        PairConfig {
            price_unit: dec!(0.01),
            min_order_size: dec!(1000),
        },
    );
    map.insert(
        "BTC_JPY".to_string(),
        PairConfig {
            price_unit: dec!(1),
            min_order_size: dec!(0.001),
        },
    );
    map
}

#[test]
fn collect_pair_decimals_picks_min_order_size_per_pair() {
    let actual = collect_pair_decimals(&two_pair_configs(), |v| v.min_order_size);

    assert_eq!(actual.get(&Pair::new("USD_JPY")), Some(&dec!(1000)));
    assert_eq!(actual.get(&Pair::new("BTC_JPY")), Some(&dec!(0.001)));
}

#[test]
fn collect_pair_decimals_picks_price_unit_per_pair() {
    let actual = collect_pair_decimals(&two_pair_configs(), |v| v.price_unit);

    assert_eq!(actual.get(&Pair::new("USD_JPY")), Some(&dec!(0.01)));
    assert_eq!(actual.get(&Pair::new("BTC_JPY")), Some(&dec!(1)));
}

#[test]
fn collect_pair_decimals_returns_empty_map_for_empty_input() {
    let empty: HashMap<String, PairConfig> = HashMap::new();

    let actual = collect_pair_decimals(&empty, |v| v.price_unit);

    assert!(actual.is_empty());
}
