//! Phase 2: Notifications, Health, Market, and Auth API tests.

use auto_trader_integration_tests::helpers::{app, db, seed};
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use chrono::{TimeZone, Utc};
use rust_decimal_macros::dec;
use serde_json::Value;

// ── GET /api/notifications ───────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_list_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/notifications"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["items"].as_array().unwrap().is_empty());
    assert_eq!(json["total"], 0);
    assert_eq!(json["unread_count"], 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_list_with_kind_filter(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "notif_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;
    let trade_id = seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(149),
        dec!(1),
        Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
    )
    .await;

    seed::seed_notification(
        &pool,
        "trade_opened",
        trade_id,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "long",
        dec!(150),
        None,
        None,
        None,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Filter by kind=trade_opened.
    let resp = client
        .get(app.endpoint("/api/notifications?kind=trade_opened"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 1);
    assert_eq!(json["items"][0]["kind"], "trade_opened");

    // Filter by kind=trade_closed -> 0 results.
    let resp = client
        .get(app.endpoint("/api/notifications?kind=trade_closed"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_invalid_kind_returns_400(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/notifications?kind=invalid_kind"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("kind"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_invalid_date_returns_400(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/notifications?from=not-a-date"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("from"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_date_filter(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Valid date filter should succeed (even if no notifications match).
    let resp = client
        .get(app.endpoint("/api/notifications?from=2026-03-01&to=2026-03-31"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["items"].is_array());
}

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_pagination(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "notif_page",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;
    let trade_id = seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(149),
        dec!(1),
        Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
    )
    .await;

    // Insert 5 notifications.
    for _ in 0..5 {
        seed::seed_notification(
            &pool,
            "trade_opened",
            trade_id,
            account_id,
            "bb_mean_revert_v1",
            "USD_JPY",
            "long",
            dec!(150),
            None,
            None,
            None,
        )
        .await;
    }

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Page 1, limit=2.
    let resp = client
        .get(app.endpoint("/api/notifications?page=1&limit=2"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 5);
    assert_eq!(json["page"], 1);
    assert_eq!(json["limit"], 2);
    assert_eq!(json["items"].as_array().unwrap().len(), 2);

    // Page 3, limit=2 -> 1 remaining.
    let resp = client
        .get(app.endpoint("/api/notifications?page=3&limit=2"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 5);
    assert_eq!(json["items"].as_array().unwrap().len(), 1);
}

// ── POST /api/notifications/mark-all-read ────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_mark_all_read(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "mark_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;
    let trade_id = seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(149),
        dec!(1),
        Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
    )
    .await;

    seed::seed_notification(
        &pool,
        "trade_opened",
        trade_id,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "long",
        dec!(150),
        None,
        None,
        None,
    )
    .await;
    seed::seed_notification(
        &pool,
        "trade_opened",
        trade_id,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "long",
        dec!(150),
        None,
        None,
        None,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .post(app.endpoint("/api/notifications/mark-all-read"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["marked"], 2);

    // Verify unread count is now 0.
    let resp = client
        .get(app.endpoint("/api/notifications/unread-count"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["count"], 0);
}

// ── GET /api/notifications/unread-count ──────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_unread_count(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "unread_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;
    let trade_id = seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(149),
        dec!(1),
        Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
    )
    .await;

    // 2 unread, 1 read.
    seed::seed_notification(
        &pool,
        "trade_opened",
        trade_id,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "long",
        dec!(150),
        None,
        None,
        None,
    )
    .await;
    seed::seed_notification(
        &pool,
        "trade_opened",
        trade_id,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "long",
        dec!(150),
        None,
        None,
        None,
    )
    .await;
    seed::seed_notification(
        &pool,
        "trade_opened",
        trade_id,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "long",
        dec!(150),
        None,
        None,
        Some(Utc::now()),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/notifications/unread-count"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["count"], 2);
}

// ── GET /api/health/market-feed ──────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn health_market_feed_no_expected_feeds(pool: sqlx::PgPool) {
    // Default spawn_test_app has empty expected feeds.
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/health/market-feed"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["feeds"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn health_market_feed_with_expected_feeds(pool: sqlx::PgPool) {
    let expected = vec![
        FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY")),
        FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY")),
    ];
    let price_store = PriceStore::new(expected);

    // Insert a fresh tick for GmoFx only.
    let now = Utc::now();
    price_store
        .update(
            FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY")),
            LatestTick {
                price: dec!(150),
                best_bid: Some(dec!(149.999)),
                best_ask: Some(dec!(150.001)),
                ts: now,
            },
        )
        .await;

    let app = app::spawn_test_app_with_price_store(pool, price_store).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/health/market-feed"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    let feeds = json["feeds"].as_array().unwrap();
    assert_eq!(feeds.len(), 2);

    let gmo = feeds.iter().find(|f| f["exchange"] == "gmo_fx").unwrap();
    assert_eq!(gmo["status"], "healthy");
    assert!(gmo["last_tick_age_secs"].is_number());

    let bf = feeds
        .iter()
        .find(|f| f["exchange"] == "bitflyer_cfd")
        .unwrap();
    assert_eq!(bf["status"], "missing");
}

// ── GET /api/market/prices ───────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn market_prices_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/market/prices"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["prices"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn market_prices_snapshot(pool: sqlx::PgPool) {
    let price_store = PriceStore::new(vec![]);
    let now = Utc::now();

    price_store
        .update(
            FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY")),
            LatestTick {
                price: dec!(150.123),
                best_bid: None,
                best_ask: None,
                ts: now,
            },
        )
        .await;

    let app = app::spawn_test_app_with_price_store(pool, price_store).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/market/prices"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    let prices = json["prices"].as_array().unwrap();
    assert_eq!(prices.len(), 1);
    assert_eq!(prices[0]["exchange"], "gmo_fx");
    assert_eq!(prices[0]["pair"], "USD_JPY");
}

// ── Auth middleware ──────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn auth_no_token_configured_allows_all(pool: sqlx::PgPool) {
    // Default: API_TOKEN env is not set -> no auth required.
    // SAFETY: test runs sequentially via sqlx::test (one DB per test).
    unsafe { std::env::remove_var("API_TOKEN") };
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_valid_token(pool: sqlx::PgPool) {
    // SAFETY: test runs sequentially via sqlx::test (one DB per test).
    unsafe { std::env::set_var("API_TOKEN", "test-secret-token") };
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .header("Authorization", "Bearer test-secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);

    unsafe { std::env::remove_var("API_TOKEN") };
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_missing_token_returns_401(pool: sqlx::PgPool) {
    // SAFETY: test runs sequentially via sqlx::test (one DB per test).
    unsafe { std::env::set_var("API_TOKEN", "test-secret-token") };
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);

    unsafe { std::env::remove_var("API_TOKEN") };
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_invalid_token_returns_401(pool: sqlx::PgPool) {
    // SAFETY: test runs sequentially via sqlx::test (one DB per test).
    unsafe { std::env::set_var("API_TOKEN", "test-secret-token") };
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);

    unsafe { std::env::remove_var("API_TOKEN") };
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_invalid_format_returns_401(pool: sqlx::PgPool) {
    // SAFETY: test runs sequentially via sqlx::test (one DB per test).
    unsafe { std::env::set_var("API_TOKEN", "test-secret-token") };
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Wrong auth scheme (Basic instead of Bearer).
    let resp = client
        .get(app.endpoint("/api/strategies"))
        .header("Authorization", "Basic test-secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);

    unsafe { std::env::remove_var("API_TOKEN") };
}
