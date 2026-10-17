use auto_trader_core::config::AppConfig;
use auto_trader_core::types::{Exchange, Pair};

use super::*;

fn feed_key(exchange: Exchange, pair: &str) -> FeedKey {
    FeedKey::new(exchange, Pair::new(pair))
}

#[test]
fn group_pairs_by_exchange_collects_unique_pairs_per_exchange() {
    let expected_feeds = vec![
        feed_key(Exchange::Oanda, "USD_JPY"),
        feed_key(Exchange::Oanda, "EUR_USD"),
        feed_key(Exchange::GmoFx, "USD_JPY"),
        feed_key(Exchange::GmoFx, "USD_JPY"),
    ];

    let actual = group_pairs_by_exchange(&expected_feeds);

    assert_eq!(
        actual.get(&Exchange::Oanda).cloned().unwrap_or_default(),
        ["USD_JPY".to_string(), "EUR_USD".to_string()]
            .into_iter()
            .collect::<HashSet<String>>()
    );
    assert_eq!(
        actual.get(&Exchange::GmoFx).cloned().unwrap_or_default(),
        ["USD_JPY".to_string()]
            .into_iter()
            .collect::<HashSet<String>>()
    );
    assert!(!actual.contains_key(&Exchange::BitflyerCfd));
}

#[test]
fn group_pairs_by_exchange_returns_empty_map_for_empty_input() {
    let actual = group_pairs_by_exchange(&[]);

    assert!(actual.is_empty());
}

fn config_with_pairs(pairs_json: serde_json::Value) -> AppConfig {
    let config_json = serde_json::json!({
        "vegapunk": {"endpoint": "http://x", "schema": "y"},
        "database": {"url": "postgresql://x"},
        "monitor": {"interval_secs": 60},
        "pairs": pairs_json,
    });
    serde_json::from_value(config_json).expect("config_with_pairs must parse")
}

#[test]
fn resolve_configured_crypto_pairs_returns_pairs_from_config_when_set() {
    let config = config_with_pairs(serde_json::json!({ "crypto": ["BTC_JPY", "ETH_JPY"] }));

    let actual = resolve_configured_crypto_pairs(&config);

    assert_eq!(actual, vec![Pair::new("BTC_JPY"), Pair::new("ETH_JPY")]);
}

#[test]
fn resolve_configured_crypto_pairs_returns_empty_when_not_configured() {
    let config = config_with_pairs(serde_json::json!({}));

    let actual = resolve_configured_crypto_pairs(&config);

    assert!(actual.is_empty());
}

#[test]
fn resolve_configured_fx_pairs_prefers_active_pairs_when_set() {
    let config = config_with_pairs(serde_json::json!({ "active": ["USD_JPY"], "fx": ["EUR_USD"] }));

    let actual = resolve_configured_fx_pairs(&config);

    assert_eq!(actual, vec![Pair::new("USD_JPY")]);
}

#[test]
fn resolve_configured_fx_pairs_falls_back_to_fx_pairs_when_active_is_empty() {
    let config = config_with_pairs(serde_json::json!({ "active": [], "fx": ["EUR_USD"] }));

    let actual = resolve_configured_fx_pairs(&config);

    assert_eq!(actual, vec![Pair::new("EUR_USD")]);
}
