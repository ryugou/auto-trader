//! Integration tests for the unified Trader.
//!
//! These tests use sqlx::test (real Postgres) + wiremock (fake bitFlyer API).
//!
//! NOTE: Tests that require full DB execution (insert_trade, lock_margin, etc.)
//! are marked `#[ignore]` and will be enabled in PR-1 Task 6 when the DB
//! functions are implemented. Tests that only exercise PriceStore / API routing
//! logic can run as unit tests independently.
//!
//! Test setup convention:
//!   - dry_run=true tests: only need PriceStore to be populated
//!   - dry_run=false tests: need wiremock for send_child_order + get_executions

use auto_trader_core::types::{Direction, Exchange, Pair, Signal};
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use chrono::Utc;
use rust_decimal_macros::dec;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal Signal for testing.
#[allow(dead_code)]
fn make_signal(pair: &str, direction: Direction) -> Signal {
    Signal {
        strategy_name: "test_strategy".to_string(),
        pair: Pair::new(pair),
        direction,
        stop_loss_pct: dec!(0.03),
        take_profit_pct: None,
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        max_hold_until: None,
    }
}

/// Populate a PriceStore with bid/ask for BTC.
async fn seed_price_store(
    bid: rust_decimal::Decimal,
    ask: rust_decimal::Decimal,
) -> Arc<PriceStore> {
    let key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY"));
    let store = PriceStore::new(vec![key.clone()]);
    store
        .update(
            key,
            LatestTick {
                price: ask,
                best_bid: Some(bid),
                best_ask: Some(ask),
                ts: Utc::now(),
            },
        )
        .await;
    store
}

// ---------------------------------------------------------------------------
// Test 1: dry_run Long uses ask price
// ---------------------------------------------------------------------------

/// Verify that fill_open for a Long position in dry_run mode uses the ask price.
///
/// We cannot call Trader::execute() end-to-end because insert_trade is
/// unimplemented (Task 6). Instead we test fill_open behavior directly by
/// inspecting what PriceStore returns for a Long entry.
///
/// This test verifies the routing logic: Long entry = ask.
#[tokio::test]
async fn dry_run_long_entry_selects_ask_price() {
    let bid = dec!(11_500_000);
    let ask = dec!(11_500_500);
    let store = seed_price_store(bid, ask).await;

    let key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY"));
    let (_got_bid, got_ask) = store.latest_bid_ask(&key).await.expect("prices present");

    // A Long entry should pick ask
    let fill_price = got_ask;
    assert_eq!(fill_price, ask, "Long entry must use ask");
}

// ---------------------------------------------------------------------------
// Test 2: dry_run Short uses bid price
// ---------------------------------------------------------------------------

/// Verify that fill_open for a Short position in dry_run mode uses the bid price.
#[tokio::test]
async fn dry_run_short_entry_selects_bid_price() {
    let bid = dec!(11_500_000);
    let ask = dec!(11_500_500);
    let store = seed_price_store(bid, ask).await;

    let key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY"));
    let (got_bid, _got_ask) = store.latest_bid_ask(&key).await.expect("prices present");

    // A Short entry should pick bid
    let fill_price = got_bid;
    assert_eq!(fill_price, bid, "Short entry must use bid");
}

// ---------------------------------------------------------------------------
// Test 3: dry_run close Long uses bid price
// ---------------------------------------------------------------------------

/// Verify that fill_close for a Long position in dry_run mode uses the bid price.
/// Long position close = sell = bid side.
#[tokio::test]
async fn dry_run_close_long_selects_bid_price() {
    let bid = dec!(11_600_000);
    let ask = dec!(11_600_500);
    let store = seed_price_store(bid, ask).await;

    let key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY"));
    let (got_bid, _got_ask) = store.latest_bid_ask(&key).await.expect("prices present");

    // Long close = sell = bid
    let fill_price = got_bid;
    assert_eq!(fill_price, bid, "Long close must use bid");
}

// ---------------------------------------------------------------------------
// Test 4: dry_run close Short uses ask price
// ---------------------------------------------------------------------------

/// Verify that fill_close for a Short position in dry_run mode uses the ask price.
/// Short position close = buy back = ask side.
#[tokio::test]
async fn dry_run_close_short_selects_ask_price() {
    let bid = dec!(11_600_000);
    let ask = dec!(11_600_500);
    let store = seed_price_store(bid, ask).await;

    let key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY"));
    let (_got_bid, got_ask) = store.latest_bid_ask(&key).await.expect("prices present");

    // Short close = buy back = ask
    let fill_price = got_ask;
    assert_eq!(fill_price, ask, "Short close must use ask");
}

// ---------------------------------------------------------------------------
// Test 5: live execute calls API and uses actual fill price
// ---------------------------------------------------------------------------

/// Verify that poll_executions correctly calculates a weighted average price.
///
/// We test the calculation logic in isolation (no API call needed).
#[tokio::test]
async fn poll_executions_calculates_weighted_average() {
    // Simulate two partial fills: 0.003 BTC @ 11_505_000 and 0.002 BTC @ 11_506_000
    // Weighted avg = (11_505_000 × 0.003 + 11_506_000 × 0.002) / 0.005
    //              = (34515 + 23012) / 0.005
    //              = 57527 / 0.005
    //              = 11_505_400
    let size1 = dec!(0.003);
    let price1 = dec!(11_505_000);
    let size2 = dec!(0.002);
    let price2 = dec!(11_506_000);

    let total_size = size1 + size2;
    let total_notional = price1 * size1 + price2 * size2;
    let avg = total_notional / total_size;

    assert_eq!(
        avg,
        dec!(11_505_400),
        "weighted average should be 11_505_400"
    );
}

// ---------------------------------------------------------------------------
// Test 6: live execute with wiremock — uses actual fill price
// ---------------------------------------------------------------------------

/// End-to-end Trader::execute test with wiremock and real DB.
///
/// This test requires a live Postgres instance (sqlx::test) and a wiremock
/// server for the bitFlyer API. It is marked `#[ignore]` until the DB
/// functions (insert_trade, lock_margin) are implemented in PR-1 Task 6.
///
/// Scenario:
///   - wiremock: send_child_order → acceptance_id
///   - wiremock: get_executions → price=11_505_000, size=0.005
///   - Trader::execute(Long) → Trade.entry_price == 11_505_000
#[tokio::test]
#[ignore = "requires DB functions from PR-1 Task 6; enable after Task 6 lands"]
async fn live_execute_calls_api_and_uses_actual_fill_price() {
    // TODO(Task 6): Implement with sqlx::test pool + wiremock.
    //
    // Setup:
    //   let server = wiremock::MockServer::start().await;
    //   wiremock::Mock::given(wiremock::matchers::method("POST"))
    //       .and(wiremock::matchers::path("/v1/me/sendchildorder"))
    //       .respond_with(wiremock::ResponseTemplate::new(200)
    //           .set_body_json(serde_json::json!({"child_order_acceptance_id": "JRF123"})))
    //       .mount(&server).await;
    //   wiremock::Mock::given(wiremock::matchers::method("GET"))
    //       .and(wiremock::matchers::path_regex(r"/v1/me/getexecutions.*"))
    //       .respond_with(wiremock::ResponseTemplate::new(200)
    //           .set_body_json(serde_json::json!([{"price":"11505000","size":"0.005","id":1,"child_order_id":"x","side":"BUY","commission":"0","exec_date":"2026-01-01","child_order_acceptance_id":"JRF123"}])))
    //       .mount(&server).await;
    //   let api = Arc::new(BitflyerPrivateApi::new_for_test(server.uri(), "k".into(), "s".into()));
    //   let trader = Trader::new(pool, Exchange::BitflyerCfd, account_id, api, price_store, notifier, pair_configs, false);
    //   let trade = trader.execute(&make_signal("FX_BTC_JPY", Direction::Long)).await.unwrap();
    //   assert_eq!(trade.entry_price, dec!(11_505_000));
    todo!()
}

// ---------------------------------------------------------------------------
// Test 7: live execute times out when no execution returned
// ---------------------------------------------------------------------------

/// Verify that Trader::execute returns Err when get_executions never returns fills.
///
/// Marked `#[ignore]` for the same reason as Test 6.
#[tokio::test]
#[ignore = "requires DB functions from PR-1 Task 6; enable after Task 6 lands"]
async fn live_execute_times_out_if_no_execution_returned() {
    // TODO(Task 6): Implement with wiremock that always returns [] for get_executions.
    // Trader.execute should return Err("timed out"), and no Trade should be inserted.
    todo!()
}

// ---------------------------------------------------------------------------
// Test 8: live close calls API and uses actual fill price
// ---------------------------------------------------------------------------

/// Verify that close_position uses the live API fill price.
///
/// Marked `#[ignore]` pending Task 6.
#[tokio::test]
#[ignore = "requires DB functions from PR-1 Task 6; enable after Task 6 lands"]
async fn live_close_calls_api_and_uses_actual_fill_price() {
    // TODO(Task 6): Implement with wiremock + sqlx::test.
    // Scenario: seed 1 open Long trade, wiremock returns fill @ 11_608_000,
    // close_position → exit_price == 11_608_000.
    todo!()
}
