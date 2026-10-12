use auto_trader_core::types::{Exchange, Pair};

use super::*;

#[test]
fn gmo_fx_feed_new_has_no_pool() {
    let pairs = vec![Pair::new("USD_JPY")];
    let feed = GmoFxFeed::new(pairs.clone(), "M5");
    assert_eq!(feed.pairs, pairs);
    assert_eq!(feed.primary_timeframe, "M5");
    assert!(feed.pool.is_none());
}

#[test]
fn exchange_gmo_fx_as_str() {
    assert_eq!(Exchange::GmoFx.as_str(), "gmo_fx");
}

#[test]
fn should_mark_market_closed_true_only_for_maintenance_status() {
    assert!(!should_mark_market_closed(0));
    assert!(should_mark_market_closed(GMO_FX_STATUS_MAINTENANCE));
    assert!(!should_mark_market_closed(1));
    assert!(!should_mark_market_closed(-1));
    assert!(!should_mark_market_closed(99));
}
