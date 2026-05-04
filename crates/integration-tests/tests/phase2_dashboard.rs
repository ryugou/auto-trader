//! Phase 2: Dashboard API tests.

use auto_trader_integration_tests::helpers::{app, db, seed};
use chrono::{NaiveDate, TimeZone, Utc};
use rust_decimal_macros::dec;
use serde_json::Value;

// ── GET /api/dashboard/summary ───────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_summary_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/summary"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["trade_count"], 0);
    assert_eq!(json["win_count"], 0);
    assert_eq!(json["loss_count"], 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_summary_with_data(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "summary_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    let date = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap();
    seed::seed_daily_summary(
        &pool,
        account_id,
        date,
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "paper",
        5,
        3,
        dec!(5000),
        dec!(1000),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/summary"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["trade_count"], 5);
    assert_eq!(json["win_count"], 3);
    assert_eq!(json["loss_count"], 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_summary_with_date_filter(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "date_filter",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    seed::seed_daily_summary(
        &pool,
        account_id,
        NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "paper",
        2,
        1,
        dec!(1000),
        dec!(500),
    )
    .await;
    seed::seed_daily_summary(
        &pool,
        account_id,
        NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "paper",
        3,
        2,
        dec!(2000),
        dec!(300),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Filter to only March 10-20 (should only include March 15).
    let resp = client
        .get(app.endpoint("/api/dashboard/summary?from=2026-03-10&to=2026-03-20"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["trade_count"], 3);
}

// ── GET /api/dashboard/pnl-history ───────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_pnl_history(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "pnl_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    seed::seed_daily_summary(
        &pool,
        account_id,
        NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "paper",
        2,
        1,
        dec!(1000),
        dec!(500),
    )
    .await;
    seed::seed_daily_summary(
        &pool,
        account_id,
        NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "paper",
        1,
        0,
        dec!(-500),
        dec!(800),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/pnl-history"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let rows: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["date"], "2026-03-01");
    assert_eq!(rows[1]["date"], "2026-03-02");
}

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_pnl_history_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/pnl-history"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let rows: Vec<Value> = resp.json().await.unwrap();
    assert!(rows.is_empty());
}

// ── GET /api/dashboard/balance-history ────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_balance_history(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/balance-history"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["accounts"].is_array());
}

// ── GET /api/dashboard/strategies ────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_strategy_stats(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "strat_stats",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    seed::seed_daily_summary(
        &pool,
        account_id,
        NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "paper",
        5,
        3,
        dec!(5000),
        dec!(1000),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let stats: Vec<Value> = resp.json().await.unwrap();
    let bb = stats
        .iter()
        .find(|s| s["strategy_name"] == "bb_mean_revert_v1");
    assert!(bb.is_some(), "should have bb_mean_revert_v1 stats");
    assert_eq!(bb.unwrap()["trade_count"], 5);
}

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_strategy_stats_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let stats: Vec<Value> = resp.json().await.unwrap();
    assert!(stats.is_empty());
}

// ── GET /api/dashboard/pairs ─────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_pair_stats(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "pair_stats",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    seed::seed_daily_summary(
        &pool,
        account_id,
        NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        "bb_mean_revert_v1",
        "USD_JPY",
        "gmo_fx",
        "paper",
        3,
        2,
        dec!(3000),
        dec!(500),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/pairs"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let stats: Vec<Value> = resp.json().await.unwrap();
    let usd = stats.iter().find(|s| s["pair"] == "USD_JPY");
    assert!(usd.is_some());
    assert_eq!(usd.unwrap()["trade_count"], 3);
}

// ── GET /api/dashboard/hourly-winrate ────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_hourly_winrate(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "hourly_test",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;

    // Seed closed trades at different hours to get hourly data.
    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    let t1_exit = Utc.with_ymd_and_hms(2026, 3, 1, 11, 0, 0).unwrap();
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
        t1_exit,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/hourly-winrate"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let rows: Vec<Value> = resp.json().await.unwrap();
    // Should have at least hour=10 entry.
    let h10 = rows.iter().find(|r| r["hour"] == 10);
    assert!(h10.is_some(), "should have hour=10 entry");
    assert_eq!(h10.unwrap()["trade_count"], 1);
    assert_eq!(h10.unwrap()["win_count"], 1);
}

// ── GET /api/dashboard/pnl-history (bad date) ───────────────────────────

/// 2.50: 不正な日付フォーマットで pnl-history を呼ぶ。
/// dashboard の parse_date は .ok() で静かに None に変換するため 200 が返る。
/// (notifications の parse_opt_date とは異なり、400 にはならない)
#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_pnl_history_bad_date_returns_200(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/pnl-history?from=not-a-date"))
        .send()
        .await
        .unwrap();

    // Dashboard silently ignores invalid dates (parse_date returns None via .ok())
    // so the response is 200 with unfiltered data.
    assert_eq!(resp.status().as_u16(), 200);
}

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_hourly_winrate_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/hourly-winrate"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let rows: Vec<Value> = resp.json().await.unwrap();
    assert!(rows.is_empty());
}
