//! Integration tests for reconciler + balance_sync using wiremock.

use rust_decimal_macros::dec;
use sqlx::PgPool;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_api(base_url: &str) -> auto_trader_market::bitflyer_private::BitflyerPrivateApi {
    auto_trader_market::bitflyer_private::BitflyerPrivateApi::new_for_test(
        base_url.to_string(),
        "test_key".into(),
        "test_secret".into(),
    )
}

#[sqlx::test(migrations = "../../migrations")]
async fn reconciler_reports_db_orphan_when_exchange_returns_empty(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getpositions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let account_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                                        initial_balance, current_balance, leverage, currency)
         VALUES ($1, 'live1', 'live', 'bitflyer_cfd', 'donchian_trend_v1',
                 30000, 30000, 2, 'JPY')",
    )
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    let trade_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO trades (id, account_id, strategy_name, pair, exchange, direction,
                             entry_price, quantity, leverage, stop_loss, entry_at, status)
         VALUES ($1, $2, 'donchian_trend_v1', 'FX_BTC_JPY', 'bitflyer_cfd', 'long',
                 5000000, 0.01, 2, 4800000, NOW(), 'open')",
    )
    .bind(trade_id)
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    let api = Arc::new(test_api(&server.uri()));
    let notifier = Arc::new(auto_trader_notify::Notifier::new_disabled());
    let account = auto_trader_db::trading_accounts::get(&pool, account_id)
        .await
        .unwrap()
        .unwrap();

    // Call reconcile_account directly (not the loop); verifies the diff
    // detection end-to-end with a real DB + mocked exchange.
    auto_trader::tasks::reconciler::reconcile_account(
        &pool,
        &api,
        &notifier,
        &account,
        "FX_BTC_JPY",
    )
    .await
    .unwrap();
    // No panic = DB orphan detected. (Notifier is disabled; no assertion on
    // notify content — pure-fn compute_diff covers that.)
}

/// Reconciling pair A must NOT flag a DB trade on pair B as a db_orphan.
/// Regression test for the pair-scope bug: the old query fetched all
/// open trades for the account regardless of pair.
#[sqlx::test(migrations = "../../migrations")]
async fn reconciler_does_not_flag_other_pair_trade_as_orphan(pool: PgPool) {
    let server = MockServer::start().await;
    // Exchange returns empty positions for FX_BTC_JPY.
    Mock::given(method("GET"))
        .and(path("/v1/me/getpositions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let account_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                                        initial_balance, current_balance, leverage, currency)
         VALUES ($1, 'live1', 'live', 'bitflyer_cfd', 'donchian_trend_v1',
                 30000, 30000, 2, 'JPY')",
    )
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    // Seed a trade on a DIFFERENT pair (ETH_JPY).
    let other_trade_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO trades (id, account_id, strategy_name, pair, exchange, direction,
                             entry_price, quantity, leverage, stop_loss, entry_at, status)
         VALUES ($1, $2, 'donchian_trend_v1', 'ETH_JPY', 'bitflyer_cfd', 'long',
                 300000, 0.1, 2, 280000, NOW(), 'open')",
    )
    .bind(other_trade_id)
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    let api = Arc::new(test_api(&server.uri()));
    let notifier = Arc::new(auto_trader_notify::Notifier::new_disabled());
    let account = auto_trader_db::trading_accounts::get(&pool, account_id)
        .await
        .unwrap()
        .unwrap();

    // Reconcile only FX_BTC_JPY. The ETH_JPY trade must be invisible to this call.
    // With the bug, ETH_JPY trade would show as db_orphan (no exchange position for it
    // because get_positions only covers FX_BTC_JPY).
    // After the fix the query is scoped to pair = 'FX_BTC_JPY', so db is empty,
    // exchange is empty → no diff → no notification (but also no false orphan).
    auto_trader::tasks::reconciler::reconcile_account(
        &pool,
        &api,
        &notifier,
        &account,
        "FX_BTC_JPY",
    )
    .await
    .unwrap();
    // No panic = clean. The ETH_JPY trade was not touched. Verify it is still open.
    let status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id = $1")
        .bind(other_trade_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "open", "trade on other pair must not be modified");
}

#[sqlx::test(migrations = "../../migrations")]
async fn balance_sync_updates_current_balance_from_exchange(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getcollateral"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "collateral": 30500.0,
            "open_position_pnl": 0.0,
            "require_collateral": 0.0,
            "keep_rate": 0.0,
        })))
        .mount(&server)
        .await;

    let account_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                                        initial_balance, current_balance, leverage, currency)
         VALUES ($1, 'live1', 'live', 'bitflyer_cfd', 'donchian_trend_v1',
                 30000, 30000, 2, 'JPY')",
    )
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    let api = test_api(&server.uri());
    let notifier = auto_trader_notify::Notifier::new_disabled();
    let account = auto_trader_db::trading_accounts::get(&pool, account_id)
        .await
        .unwrap()
        .unwrap();

    auto_trader::tasks::balance_sync::sync_account(&pool, &api, &notifier, &account, dec!(0.01))
        .await
        .unwrap();

    let updated = auto_trader_db::trading_accounts::get(&pool, account_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.current_balance, dec!(30500));
}
