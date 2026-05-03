//! Phase 2: Strategies API tests.

use auto_trader_integration_tests::helpers::app;
use serde_json::Value;

// ── GET /api/strategies ──────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn strategies_list(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let strategies: Vec<Value> = resp.json().await.unwrap();
    // Migrations seed strategies; at least 4 standard ones should exist.
    assert!(
        strategies.len() >= 4,
        "expected at least 4 strategies from migration seeds, got {}",
        strategies.len()
    );
    // Check each has required fields.
    for s in &strategies {
        assert!(s["name"].is_string());
        assert!(s["display_name"].is_string());
        assert!(s["category"].is_string());
        assert!(s["risk_level"].is_string());
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn strategies_list_with_category_filter(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies?category=crypto"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let strategies: Vec<Value> = resp.json().await.unwrap();
    for s in &strategies {
        assert_eq!(s["category"], "crypto");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn strategies_list_with_fx_category_filter(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies?category=fx"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let strategies: Vec<Value> = resp.json().await.unwrap();
    for s in &strategies {
        assert_eq!(s["category"], "fx");
    }
}

// ── GET /api/strategies/:name ────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn strategies_get_one(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies/bb_mean_revert_v1"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["name"], "bb_mean_revert_v1");
}

#[sqlx::test(migrations = "../../migrations")]
async fn strategies_get_one_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies/nonexistent_xyz"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("not found"));
}
