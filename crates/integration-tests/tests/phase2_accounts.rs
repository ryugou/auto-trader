//! Phase 2: Trading accounts CRUD API tests.

use auto_trader_integration_tests::helpers::{app, seed};
use chrono::Utc;
use rust_decimal_macros::dec;
use serde_json::{Value, json};

// ── POST /api/trading-accounts ───────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn create_paper_account(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Test Paper",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 201);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["name"], "Test Paper");
    assert_eq!(json["exchange"], "gmo_fx");
    assert_eq!(json["account_type"], "paper");
    assert_eq!(json["strategy"], "bb_mean_revert_v1");
    assert!(json["id"].is_string());
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_live_account(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Test Live",
        "exchange": "bitflyer_cfd",
        "initial_balance": 50000,
        "leverage": 1,
        "strategy": "bb_mean_revert_v1",
        "account_type": "live"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 201);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["account_type"], "live");
    assert_eq!(json["exchange"], "bitflyer_cfd");
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_paper_account_oanda(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Oanda Paper",
        "exchange": "oanda",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "donchian_trend_v1",
        "account_type": "paper"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 201);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["exchange"], "oanda");
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_invalid_account_type(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Bad Type",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "invalid"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("account_type"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_duplicate_name(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Dup Name",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });

    // First create succeeds.
    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Second create with same name.
    // NOTE: trading_accounts table does NOT have UNIQUE(name) in current schema,
    // so duplicate names are allowed. We just verify the second create also succeeds.
    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_invalid_exchange(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Bad Exchange",
        "exchange": "unknown_exchange",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
}

/// Defense in depth: an exchange that has no [exchange_margin.<name>] entry
/// must be rejected at the API layer. Worker tasks (signal/exit/close) now
/// log+skip on a miss via `startup::liquidation_level_or_log`, but failing
/// the create here surfaces the configuration gap immediately instead of
/// silently dropping every signal/exit on the new account at runtime.
#[sqlx::test(migrations = "../../migrations")]
async fn create_account_rejected_when_exchange_missing_from_margin_config(pool: sqlx::PgPool) {
    use auto_trader_core::types::Exchange;
    use auto_trader_market::price_store::PriceStore;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use std::sync::Arc;

    // Levels map intentionally only contains bitflyer_cfd — gmo_fx is missing.
    let mut levels: HashMap<Exchange, rust_decimal::Decimal> = HashMap::new();
    levels.insert(Exchange::BitflyerCfd, dec!(0.50));
    let app =
        app::spawn_test_app_with_levels(pool, PriceStore::new(vec![]), Arc::new(levels)).await;
    let client = app.client();

    let body = json!({
        "name": "Missing Margin Config",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    let error = json["error"].as_str().unwrap_or("");
    assert!(
        error.contains("exchange_margin"),
        "error must reference [exchange_margin] section, got: {error}"
    );
    assert!(
        error.contains("gmo_fx"),
        "error must mention the offending exchange, got: {error}"
    );
}

/// Defense in depth: even when the exchange is *present* in the levels map,
/// a non-positive `liquidation_margin_level` must reject account creation.
/// This catches the "first account on a previously-unused exchange whose
/// config was left at 0/negative" footgun that the startup gate cannot.
#[sqlx::test(migrations = "../../migrations")]
async fn create_account_rejected_when_liquidation_margin_level_non_positive(pool: sqlx::PgPool) {
    use auto_trader_core::types::Exchange;
    use auto_trader_market::price_store::PriceStore;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use std::sync::Arc;

    // Levels map contains gmo_fx but with a sentinel-bad value (0).
    let mut levels: HashMap<Exchange, rust_decimal::Decimal> = HashMap::new();
    levels.insert(Exchange::BitflyerCfd, dec!(0.50));
    levels.insert(Exchange::GmoFx, dec!(0));
    let app =
        app::spawn_test_app_with_levels(pool, PriceStore::new(vec![]), Arc::new(levels)).await;
    let client = app.client();

    let body = json!({
        "name": "Bad Y For This Exchange",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 10,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    let error = json["error"].as_str().unwrap_or("");
    assert!(
        error.contains("liquidation_margin_level"),
        "error must reference liquidation_margin_level, got: {error}"
    );
    assert!(
        error.contains("gmo_fx"),
        "error must mention the offending exchange, got: {error}"
    );
    assert!(
        error.contains("> 0") || error.contains("must be > 0"),
        "error must explain the > 0 constraint, got: {error}"
    );
}

/// 2.7a: JPY 最低残高未満 → 400。
/// validate_initial_balance は JPY の場合のみ最低残高チェックを行う。
#[sqlx::test(migrations = "../../migrations")]
async fn create_account_invalid_currency_low_balance(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // JPY with balance below minimum (10000) → 400
    let body = json!({
        "name": "Low JPY",
        "exchange": "gmo_fx",
        "initial_balance": 100,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper",
        "currency": "JPY"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("initial_balance"));
}

/// 2.7b: 未知の通貨コード "XYZ" + 有効な残高 → 201 (バリデーションなし)。
/// 現在の実装では非 JPY 通貨は検証なしで受け入れられる。
/// 将来的に通貨ホワイトリストを導入する場合はこのテストを更新する。
#[sqlx::test(migrations = "../../migrations")]
async fn create_account_unknown_currency_accepted(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "XYZ Currency",
        "exchange": "gmo_fx",
        "initial_balance": 50000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper",
        "currency": "XYZ"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    // Unknown currencies are accepted — no whitelist validation exists.
    assert_eq!(resp.status().as_u16(), 201);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["currency"], "XYZ");
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_nonexistent_strategy(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Bad Strategy",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "nonexistent_strategy_xyz",
        "account_type": "paper"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("strategy"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_insufficient_balance(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Low Balance",
        "exchange": "gmo_fx",
        "initial_balance": 100,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper",
        "currency": "JPY"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("initial_balance"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_live_account_duplicate_exchange(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Live 1",
        "exchange": "bitflyer_cfd",
        "initial_balance": 50000,
        "leverage": 1,
        "strategy": "bb_mean_revert_v1",
        "account_type": "live"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Second live account for same exchange -> error.
    let body2 = json!({
        "name": "Live 2",
        "exchange": "bitflyer_cfd",
        "initial_balance": 50000,
        "leverage": 1,
        "strategy": "bb_mean_revert_v1",
        "account_type": "live"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body2)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 409);
}

// ── GET /api/trading-accounts ────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn list_accounts_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/trading-accounts"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json.is_array(), "response should be an array");
}

#[sqlx::test(migrations = "../../migrations")]
async fn list_accounts_includes_evaluated_balance(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create an account via API.
    let body = json!({
        "name": "Eval Balance Test",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    let resp = client
        .get(app.endpoint("/api/trading-accounts"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let accounts: Vec<Value> = resp.json().await.unwrap();
    let test_account = accounts
        .iter()
        .find(|a| a["name"] == "Eval Balance Test")
        .expect("test account should be in list");
    // Decimal values may serialize as strings or numbers depending on serde config.
    assert!(
        test_account["evaluated_balance"].is_number()
            || test_account["evaluated_balance"].is_string(),
        "evaluated_balance should be present: {:?}",
        test_account["evaluated_balance"]
    );
    assert!(
        test_account["unrealized_pnl"].is_number() || test_account["unrealized_pnl"].is_string(),
        "unrealized_pnl should be present: {:?}",
        test_account["unrealized_pnl"]
    );
}

// ── GET /api/trading-accounts/:id ────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn get_account_by_id(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create account.
    let body = json!({
        "name": "Get By ID",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    let created: Value = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client
        .get(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["id"], id);
    assert_eq!(json["name"], "Get By ID");
}

#[sqlx::test(migrations = "../../migrations")]
async fn get_account_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let fake_id = uuid::Uuid::new_v4();

    let resp = client
        .get(app.endpoint(&format!("/api/trading-accounts/{fake_id}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
}

// ── PUT /api/trading-accounts/:id ────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn update_account(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create account.
    let body = json!({
        "name": "Before Update",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    let created: Value = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    // Update name and leverage.
    let update_body = json!({
        "name": "After Update",
        "leverage": 5
    });
    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&update_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["name"], "After Update");
    // Leverage is NUMERIC/Decimal, which may serialize as string "5".
    let leverage_str = json["leverage"].to_string().replace('"', "");
    assert_eq!(leverage_str, "5");
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let fake_id = uuid::Uuid::new_v4();

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{fake_id}")))
        .json(&json!({"name": "No Such Account"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
}

// ── DELETE /api/trading-accounts/:id ─────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn delete_account(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create account.
    let body = json!({
        "name": "To Delete",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    let created: Value = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client
        .delete(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 204);

    // Verify it's gone.
    let resp = client
        .get(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_account_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let fake_id = uuid::Uuid::new_v4();

    let resp = client
        .delete(app.endpoint(&format!("/api/trading-accounts/{fake_id}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_account_with_trades_fails(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create account via API.
    let body = json!({
        "name": "Has Trades",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    let created: Value = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id_str = created["id"].as_str().unwrap();
    let account_id: uuid::Uuid = id_str.parse().unwrap();

    // Seed a trade for this account.
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

    // Delete should fail due to FK constraint.
    let resp = client
        .delete(app.endpoint(&format!("/api/trading-accounts/{id_str}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 409);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("trades"));
}

// ── active / retirement (#102, #100) ─────────────────────────────────────

async fn create_test_account(client: &reqwest::Client, app: &app::TestApp, body: Value) -> Value {
    client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// GET /api/trading-accounts and GET /:id must report `active` on every row.
#[sqlx::test(migrations = "../../migrations")]
async fn account_responses_include_active_field(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let created = create_test_account(
        &client,
        &app,
        json!({
            "name": "Active Field Test",
            "exchange": "gmo_fx",
            "initial_balance": 100000,
            "leverage": 2,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }),
    )
    .await;
    assert_eq!(created["active"], true, "freshly created account is active");
    let id = created["id"].as_str().unwrap();

    let list: Vec<Value> = client
        .get(app.endpoint("/api/trading-accounts"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed = list
        .iter()
        .find(|a| a["id"] == created["id"])
        .expect("account present in list");
    assert_eq!(listed["active"], true);

    let fetched: Value = client
        .get(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched["active"], true);
}

/// Default list hides a retired account; `include_inactive=true` reveals it.
#[sqlx::test(migrations = "../../migrations")]
async fn list_accounts_hides_inactive_unless_requested(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let created = create_test_account(
        &client,
        &app,
        json!({
            "name": "Will Retire",
            "exchange": "gmo_fx",
            "initial_balance": 100000,
            "leverage": 2,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&json!({"active": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let updated: Value = resp.json().await.unwrap();
    assert_eq!(updated["active"], false);

    let default_list: Vec<Value> = client
        .get(app.endpoint("/api/trading-accounts"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        default_list.iter().all(|a| a["id"] != created["id"]),
        "retired account must be hidden from the default list"
    );

    let full_list: Vec<Value> = client
        .get(app.endpoint("/api/trading-accounts?include_inactive=true"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        full_list.iter().any(|a| a["id"] == created["id"]),
        "include_inactive=true must reveal the retired account"
    );
}

/// A paper account can be retired (`active: false`) and then reinstated
/// (`active: true`); each response must reflect the new `active` value.
#[sqlx::test(migrations = "../../migrations")]
async fn update_account_can_retire_and_reinstate(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let created = create_test_account(
        &client,
        &app,
        json!({
            "name": "Retire Reinstate",
            "exchange": "gmo_fx",
            "initial_balance": 100000,
            "leverage": 2,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let retired: Value = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&json!({"active": false}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retired["active"], false);

    let reinstated: Value = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&json!({"active": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reinstated["active"], true);
}

/// Retiring an account with an open trade is rejected (409) and the message
/// reports how many open trades are blocking it.
#[sqlx::test(migrations = "../../migrations")]
async fn update_account_retire_rejected_with_open_trades(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    let created = create_test_account(
        &client,
        &app,
        json!({
            "name": "Has Open Trade",
            "exchange": "gmo_fx",
            "initial_balance": 100000,
            "leverage": 2,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }),
    )
    .await;
    let id_str = created["id"].as_str().unwrap();
    let account_id: uuid::Uuid = id_str.parse().unwrap();

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

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id_str}")))
        .json(&json!({"active": false}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 409);
    let json: Value = resp.json().await.unwrap();
    let error = json["error"].as_str().unwrap();
    assert!(
        error.contains("1 open/closing trade"),
        "unexpected error: {error}"
    );
}

/// Retiring an account whose only unsettled trade is `closing` (exit order
/// placed, not yet confirmed filled) is rejected the same way an `open`
/// trade would be. `seed::seed_open_trade` always inserts `status = 'open'`,
/// so this inserts the row directly to get a `closing` status.
#[sqlx::test(migrations = "../../migrations")]
async fn update_account_retire_rejected_with_closing_trade(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    let created = create_test_account(
        &client,
        &app,
        json!({
            "name": "Has Closing Trade",
            "exchange": "gmo_fx",
            "initial_balance": 100000,
            "leverage": 2,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }),
    )
    .await;
    let id_str = created["id"].as_str().unwrap();
    let account_id: uuid::Uuid = id_str.parse().unwrap();

    sqlx::query(
        r#"INSERT INTO trades
               (id, account_id, strategy_name, pair, exchange, direction,
                entry_price, stop_loss, quantity, leverage, fees, status, entry_at)
           VALUES ($1, $2, 'bb_mean_revert_v1', 'USD_JPY', 'gmo_fx', 'long',
                   150, 149, 1, 2, 0, 'closing', $3)"#,
    )
    .bind(uuid::Uuid::new_v4())
    .bind(account_id)
    .bind(Utc::now())
    .execute(&pool)
    .await
    .expect("seed closing trade");

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id_str}")))
        .json(&json!({"active": false}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 409);
    let json: Value = resp.json().await.unwrap();
    let error = json["error"].as_str().unwrap();
    assert!(
        error.contains("1 open/closing trade"),
        "unexpected error: {error}"
    );
}

/// Reactivating a retired live account is rejected (409) when another live
/// account is already active on the same exchange.
#[sqlx::test(migrations = "../../migrations")]
async fn update_account_reactivate_live_rejected_when_another_is_active(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let live_body = |name: &str| {
        json!({
            "name": name,
            "exchange": "bitflyer_cfd",
            "initial_balance": 50000,
            "leverage": 1,
            "strategy": "bb_mean_revert_v1",
            "account_type": "live"
        })
    };

    // First live account, then retire it so a second can be created.
    let first = create_test_account(&client, &app, live_body("Live First")).await;
    let first_id = first["id"].as_str().unwrap();
    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{first_id}")))
        .json(&json!({"active": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&live_body("Live Second"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        201,
        "second live account should succeed once first is retired"
    );

    // Reactivating the first must now conflict with the second (active).
    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{first_id}")))
        .json(&json!({"active": true}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 409);
    let json: Value = resp.json().await.unwrap();
    assert!(
        json["error"].as_str().unwrap().contains("already active"),
        "unexpected error: {}",
        json["error"]
    );
}

/// Reinstating (`active: false` → `true`) a live account must pass the same
/// liquidation-level guard `create` enforces: the exchange needs a
/// `[exchange_margin.<name>]` entry in config. Creates and retires the
/// account while gmo_fx is configured, then reconnects with a config where
/// gmo_fx has been removed from `[exchange_margin]` (e.g. an operator
/// deleted the stale section after retiring the account) and confirms
/// reinstatement is rejected instead of producing an account PositionSizer
/// would silently refuse to size for.
#[sqlx::test(migrations = "../../migrations")]
async fn update_account_reactivate_rejected_when_exchange_missing_from_margin_config(
    pool: sqlx::PgPool,
) {
    use auto_trader_core::types::Exchange;
    use auto_trader_market::price_store::PriceStore;
    use std::collections::HashMap;
    use std::sync::Arc;

    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    let created = create_test_account(
        &client,
        &app,
        json!({
            "name": "Reinstate Missing Margin",
            "exchange": "gmo_fx",
            "initial_balance": 100000,
            "leverage": 2,
            "strategy": "bb_mean_revert_v1",
            "account_type": "live"
        }),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&json!({"active": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    drop(app);

    // Levels map intentionally only contains bitflyer_cfd — gmo_fx is missing.
    let mut levels: HashMap<Exchange, rust_decimal::Decimal> = HashMap::new();
    levels.insert(Exchange::BitflyerCfd, dec!(0.50));
    let app =
        app::spawn_test_app_with_levels(pool, PriceStore::new(vec![]), Arc::new(levels)).await;
    let client = app.client();

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&json!({"active": true}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    let error = json["error"].as_str().unwrap_or("");
    assert!(
        error.contains("exchange_margin"),
        "error must reference [exchange_margin] section, got: {error}"
    );
    assert!(
        error.contains("gmo_fx"),
        "error must mention the offending exchange, got: {error}"
    );
}

/// Mirror-image happy path: reinstating a (non-live) account succeeds when
/// its exchange has a valid (positive) `liquidation_margin_level` entry,
/// confirming the guard applies regardless of `account_type` and does not
/// regress the existing retire/reinstate flow.
#[sqlx::test(migrations = "../../migrations")]
async fn update_account_reactivate_allowed_when_liquidation_margin_level_configured(
    pool: sqlx::PgPool,
) {
    use auto_trader_core::types::Exchange;
    use auto_trader_market::price_store::PriceStore;
    use std::collections::HashMap;
    use std::sync::Arc;

    let mut levels: HashMap<Exchange, rust_decimal::Decimal> = HashMap::new();
    levels.insert(Exchange::GmoFx, dec!(1.00));
    let app =
        app::spawn_test_app_with_levels(pool, PriceStore::new(vec![]), Arc::new(levels)).await;
    let client = app.client();

    let created = create_test_account(
        &client,
        &app,
        json!({
            "name": "Reinstate Allowed",
            "exchange": "gmo_fx",
            "initial_balance": 100000,
            "leverage": 2,
            "strategy": "bb_mean_revert_v1",
            "account_type": "paper"
        }),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&json!({"active": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&json!({"active": true}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["active"], true);
}
