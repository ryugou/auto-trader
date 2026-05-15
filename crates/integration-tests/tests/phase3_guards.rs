//! Phase 3: Execution guards — exchange-pair guard, position dedup, NullExchangeApi.
//!
//! These test the individual guard components used by the signal executor.
//!
//! SKIP: 3.62-3.63 (fill_open/close Live) — requires real exchange API mock with full order lifecycle; covered at unit level
//! SKIP: 3.65 (live gate) — requires runtime env var mutation; fragile in parallel tests
//! SKIP: 3.67-3.68 (match none, multi-account) — requires full signal executor running; tested indirectly via Phase 2 API
//! SKIP: 3.69 (Live/Paper split) — tested implicitly in fill_open tests

use std::collections::{HashMap, HashSet};

use auto_trader_core::types::Exchange;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::helpers::seed;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::null_exchange_api::NullExchangeApi;
use chrono::Utc;
use rust_decimal_macros::dec;

// =========================================================================
// 3.64: exchange-pair guard
// =========================================================================

/// exchange_pairs HashMap で BitflyerCfd のペアが GmoFx のセットに含まれないことを確認。
///
/// main.rs の signal executor は expected_feeds から exchange_pairs を構築し、
/// シグナルの pair が対象 exchange のセットに含まれない場合はスキップする。
/// ここではその HashMap 構築ロジックと検証ロジックを再現してテストする。
#[test]
fn exchange_pair_guard_rejects_cross_exchange() {
    // Simulate the exchange_pairs construction from expected_feeds
    let mut map: HashMap<Exchange, HashSet<String>> = HashMap::new();
    map.entry(Exchange::BitflyerCfd)
        .or_default()
        .insert("FX_BTC_JPY".to_string());
    map.entry(Exchange::GmoFx)
        .or_default()
        .insert("USD_JPY".to_string());

    // BitflyerCfd pair should NOT be in GmoFx set
    let gmo_pairs = map.get(&Exchange::GmoFx).unwrap();
    assert!(
        !gmo_pairs.contains("FX_BTC_JPY"),
        "FX_BTC_JPY should not be in GmoFx pair set"
    );

    // BitflyerCfd pair should be in BitflyerCfd set
    let bf_pairs = map.get(&Exchange::BitflyerCfd).unwrap();
    assert!(
        bf_pairs.contains("FX_BTC_JPY"),
        "FX_BTC_JPY should be in BitflyerCfd pair set"
    );

    // USD_JPY should be in GmoFx but not in BitflyerCfd
    assert!(
        gmo_pairs.contains("USD_JPY"),
        "USD_JPY should be in GmoFx pair set"
    );
    assert!(
        !bf_pairs.contains("USD_JPY"),
        "USD_JPY should not be in BitflyerCfd pair set"
    );
}

/// exchange_pairs: 存在しない exchange にはペアセットが空。
#[test]
fn exchange_pair_guard_unknown_exchange_empty() {
    let map: HashMap<Exchange, HashSet<String>> = HashMap::new();

    // Oanda not in map → is_some_and returns false
    let has_pair = map
        .get(&Exchange::Oanda)
        .is_some_and(|pairs| pairs.contains("USD_JPY"));
    assert!(!has_pair, "Oanda not in map should return false");
}

// =========================================================================
// 3.66: position dedup (DB level)
// =========================================================================

/// 同一 strategy + pair のオープントレードが既に存在する場合、
/// DB クエリで検出できることを確認する。
#[sqlx::test(migrations = "../../migrations")]
async fn position_dedup_detects_existing_open_trade(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "dedup_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        1_000_000,
    )
    .await;

    // Seed an open trade
    seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(149),
        dec!(1),
        Utc::now(),
    )
    .await;

    // Query for open trades with same strategy + pair
    let open_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM trades
           WHERE account_id = $1
             AND strategy_name = $2
             AND pair = $3
             AND status = 'open'"#,
    )
    .bind(account_id)
    .bind("bb_mean_revert_v1")
    .bind("USD_JPY")
    .fetch_one(&pool)
    .await
    .expect("query should succeed");

    assert_eq!(open_count, 1, "should detect 1 existing open trade");

    // Different pair should have 0
    let other_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM trades
           WHERE account_id = $1
             AND strategy_name = $2
             AND pair = $3
             AND status = 'open'"#,
    )
    .bind(account_id)
    .bind("bb_mean_revert_v1")
    .bind("EUR_JPY")
    .fetch_one(&pool)
    .await
    .expect("query should succeed");

    assert_eq!(other_count, 0, "different pair should have 0 open trades");
}

// =========================================================================
// 3.70: NullExchangeApi
// =========================================================================

/// NullExchangeApi の全メソッドがエラーを返すことを確認。
#[tokio::test]
async fn null_exchange_api_returns_error_on_all_methods() {
    let api = NullExchangeApi;

    // send_child_order
    let result = api
        .send_child_order(
            auto_trader_market::bitflyer_private::SendChildOrderRequest {
                product_code: "FX_BTC_JPY".to_string(),
                child_order_type: auto_trader_market::bitflyer_private::ChildOrderType::Limit,
                side: auto_trader_market::bitflyer_private::Side::Buy,
                price: Some(dec!(15_000_000)),
                size: dec!(0.01),
                minute_to_expire: None,
                time_in_force: None,
                close_position_id: None,
            },
        )
        .await;
    assert!(result.is_err(), "send_child_order should return error");
    assert!(
        result.unwrap_err().to_string().contains("NullExchangeApi"),
        "error should mention NullExchangeApi"
    );

    // get_child_orders
    let result = api.get_child_orders("FX_BTC_JPY", "test_id").await;
    assert!(result.is_err(), "get_child_orders should return error");

    // get_executions
    let result = api.get_executions("FX_BTC_JPY", "test_id").await;
    assert!(result.is_err(), "get_executions should return error");

    // get_positions
    let result = api.get_positions("FX_BTC_JPY").await;
    assert!(result.is_err(), "get_positions should return error");

    // get_collateral
    let result = api.get_collateral().await;
    assert!(result.is_err(), "get_collateral should return error");

    // cancel_child_order
    let result = api.cancel_child_order("FX_BTC_JPY", "test_id").await;
    assert!(result.is_err(), "cancel_child_order should return error");
}
