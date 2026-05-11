//! Phase 2: Trades and Positions API tests.

use auto_trader_integration_tests::helpers::{app, db, seed};
use chrono::{TimeZone, Utc};
use rust_decimal_macros::dec;
use serde_json::Value;

// ── GET /api/trades ──────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/trades"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["trades"].is_array());
    assert_eq!(json["total"], 0);
    assert_eq!(json["page"], 1);
    assert_eq!(json["per_page"], 50);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_filter_by_status(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "filter_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();

    seed::seed_closed_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(151),
        dec!(1000),
        dec!(1),
        dec!(0),
        t1,
        t2,
    )
    .await;

    seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "FX_BTC_JPY",
        "gmo_fx",
        "short",
        dec!(5000000),
        dec!(5100000),
        dec!(0.01),
        t1,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Filter by status=closed.
    let resp = client
        .get(app.endpoint("/api/trades?status=closed"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 1);
    assert_eq!(json["trades"][0]["pair"], "USD_JPY");
    assert_eq!(json["trades"][0]["status"], "closed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_filter_by_pair(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "pair_filter",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();

    seed::seed_closed_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(151),
        dec!(1000),
        dec!(1),
        dec!(0),
        t1,
        t2,
    )
    .await;

    seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "FX_BTC_JPY",
        "gmo_fx",
        "short",
        dec!(5000000),
        dec!(5100000),
        dec!(0.01),
        t1,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/trades?pair=FX_BTC_JPY"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 1);
    assert_eq!(json["trades"][0]["pair"], "FX_BTC_JPY");
    assert_eq!(json["trades"][0]["status"], "open");
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_filter_by_exchange(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "exchange_filter",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();

    seed::seed_closed_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(151),
        dec!(1000),
        dec!(1),
        dec!(0),
        t1,
        t2,
    )
    .await;

    seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "FX_BTC_JPY",
        "gmo_fx",
        "short",
        dec!(5000000),
        dec!(5100000),
        dec!(0.01),
        t1,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/trades?exchange=gmo_fx"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_filter_by_account(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "account_filter",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();

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
        t1,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint(&format!("/api/trades?account_id={account_id}")))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_filter_by_strategy(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "strategy_filter",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();

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
        t1,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Existing strategy returns the trade.
    let resp = client
        .get(app.endpoint("/api/trades?strategy=bb_mean_revert_v1"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 1);

    // Non-matching strategy returns zero.
    let resp = client
        .get(app.endpoint("/api/trades?strategy=nonexistent"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_pagination(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "page_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    let base_time = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    // Insert 5 closed trades.
    for i in 0..5 {
        let entry_at = base_time + chrono::Duration::hours(i);
        let exit_at = entry_at + chrono::Duration::hours(1);
        seed::seed_closed_trade(
            &pool,
            account_id,
            "bb_mean_revert_v1",
            "USD_JPY",
            "gmo_fx",
            "long",
            dec!(150),
            dec!(151),
            dec!(1000),
            dec!(1),
            dec!(0),
            entry_at,
            exit_at,
        )
        .await;
    }

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Page 1, per_page=2.
    let resp = client
        .get(app.endpoint("/api/trades?page=1&per_page=2"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 5);
    assert_eq!(json["page"], 1);
    assert_eq!(json["per_page"], 2);
    assert_eq!(json["trades"].as_array().unwrap().len(), 2);

    // Page 3, per_page=2 -> 1 trade remaining.
    let resp = client
        .get(app.endpoint("/api/trades?page=3&per_page=2"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 5);
    assert_eq!(json["trades"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_total_count_accuracy(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "total_count",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    let base_time = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    for i in 0..7 {
        let entry_at = base_time + chrono::Duration::hours(i);
        let exit_at = entry_at + chrono::Duration::hours(1);
        seed::seed_closed_trade(
            &pool,
            account_id,
            "bb_mean_revert_v1",
            "USD_JPY",
            "gmo_fx",
            "long",
            dec!(150),
            dec!(151),
            dec!(1000),
            dec!(1),
            dec!(0),
            entry_at,
            exit_at,
        )
        .await;
    }

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Total should be 7 regardless of page size.
    let resp = client
        .get(app.endpoint("/api/trades?per_page=3"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 7);
    assert_eq!(json["trades"].as_array().unwrap().len(), 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_page_zero_treated_as_one(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/trades?page=0"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["page"], 1, "page=0 should be clamped to 1");
}

// ── GET /api/trades/:id/events ───────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn trade_events_for_existing_trade(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "events_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;
    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();
    let trade_id = seed::seed_closed_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(151),
        dec!(1000),
        dec!(1),
        dec!(0),
        t1,
        t2,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint(&format!("/api/trades/{trade_id}/events")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["events"].is_array());
}

#[sqlx::test(migrations = "../../migrations")]
async fn trade_events_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let fake_id = uuid::Uuid::new_v4();

    let resp = client
        .get(app.endpoint(&format!("/api/trades/{fake_id}/events")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
}

// ── GET /api/positions ───────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn positions_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/positions"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Vec<Value> = resp.json().await.unwrap();
    assert!(json.is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn positions_lists_open_trades(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "pos_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

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
        Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/positions"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(json.len(), 1);
    assert_eq!(json[0]["pair"], "USD_JPY");
    assert_eq!(json[0]["direction"], "long");
    assert_eq!(json[0]["account_name"], "pos_test");
}

#[sqlx::test(migrations = "../../migrations")]
async fn positions_excludes_closed_trades(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "closed_pos",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();

    // Only a closed trade -- no open positions.
    seed::seed_closed_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(151),
        dec!(1000),
        dec!(1),
        dec!(0),
        t1,
        t2,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/positions"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Vec<Value> = resp.json().await.unwrap();
    assert!(
        json.is_empty(),
        "closed trades should not appear in positions"
    );
}
