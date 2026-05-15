//! Phase 3: Accounts API leverage validation (Japan FSA regulatory caps).
//!
//! These tests exercise the HTTP boundary, not the db-layer helper directly.
//! They prove that:
//!  - `POST /api/trading-accounts` rejects leverage above the cap with a 400
//!    whose body mentions the cap value.
//!  - `PUT /api/trading-accounts/:id` rejects leverage updates that would
//!    push the account above the cap.
//!  - Leverage at or below the cap is accepted (201/200).

use auto_trader_integration_tests::helpers::app;
use serde_json::{Value, json};

#[sqlx::test(migrations = "../../migrations")]
async fn create_gmo_fx_account_with_leverage_30_returns_400(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&json!({
            "name": "gmo_too_high",
            "exchange": "gmo_fx",
            "initial_balance": 100_000,
            "leverage": 30,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(msg.contains("25"), "error mentions cap: {msg}");
    assert!(msg.contains("gmo_fx"), "error mentions exchange: {msg}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_bitflyer_cfd_account_with_leverage_3_returns_400(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&json!({
            "name": "bf_too_high",
            "exchange": "bitflyer_cfd",
            "initial_balance": 100_000,
            "leverage": 3,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(msg.contains("2"), "error mentions cap: {msg}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_gmo_fx_account_with_leverage_25_returns_201(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&json!({
            "name": "gmo_at_cap",
            "exchange": "gmo_fx",
            "initial_balance": 100_000,
            "leverage": 25,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_gmo_fx_account_to_leverage_30_returns_400(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let create = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&json!({
            "name": "gmo_for_update",
            "exchange": "gmo_fx",
            "initial_balance": 100_000,
            "leverage": 25,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status().as_u16(), 201);
    let body: Value = create.json().await.unwrap();
    let id = body["id"].as_str().unwrap();

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&json!({ "leverage": 30 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(msg.contains("25"), "error mentions cap: {msg}");
}
