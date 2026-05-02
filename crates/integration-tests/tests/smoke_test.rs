use auto_trader_integration_tests::helpers::{db, fixture_loader};

#[sqlx::test(migrations = "../../migrations")]
async fn db_helper_snapshot_returns_table_contents(pool: sqlx::PgPool) {
    // FK 制約: trading_accounts.strategy → strategies.name
    // migrations が strategy seed を含むため明示挿入は不要だが、
    // テスト独立性のため ensure_strategy 経由の seed_trading_account を使わず
    // 直接 INSERT する場合は strategy 行が必要。
    // ここでは migrations が seed 済みの strategy を使って直接 INSERT する。
    sqlx::query(
        r#"INSERT INTO trading_accounts
               (id, name, account_type, exchange, strategy,
                initial_balance, current_balance, leverage, currency)
           VALUES (gen_random_uuid(), 'smoke', 'paper', 'gmo_fx', 'bb_mean_revert_v1',
                   100000, 100000, 2, 'JPY')"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let snapshot = db::snapshot_tables(&pool, &["trading_accounts"]).await;
    assert!(
        snapshot.contains("smoke"),
        "snapshot must contain seeded account name: {snapshot}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn fixture_loader_inserts_candles(pool: sqlx::PgPool) {
    let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/smoke_test.csv");
    let count = fixture_loader::load_price_candles(
        &pool, "gmo_fx", "USD_JPY", "M5", &fixture_path,
    )
    .await
    .unwrap();
    assert_eq!(count, 3);

    let (row_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM price_candles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count, 3);
}

#[tokio::test]
async fn tracing_capture_records_log_lines() {
    use auto_trader_integration_tests::helpers::failure_output::TracingCapture;
    use tracing_subscriber::prelude::*;

    let (layer, buffer) = TracingCapture::new();
    let _guard = tracing_subscriber::registry().with(layer).set_default();

    tracing::info!("hello from test");
    tracing::warn!("warning message");

    let logs = buffer.lock().unwrap();
    assert!(logs.len() >= 2);
    assert!(logs.iter().any(|l| l.contains("hello from test")));
    assert!(logs.iter().any(|l| l.contains("warning message")));
}

#[tokio::test]
async fn mock_exchange_api_returns_configured_response() {
    use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
    use auto_trader_market::bitflyer_private::{ExchangePosition, SendChildOrderResponse};
    use auto_trader_market::exchange_api::ExchangeApi;
    use rust_decimal_macros::dec;

    let mock = MockExchangeApiBuilder::new()
        .with_send_child_order_response(SendChildOrderResponse {
            child_order_acceptance_id: "test-123".to_string(),
        })
        .with_get_positions_response(vec![ExchangePosition {
            product_code: "FX_BTC_JPY".to_string(),
            side: "BUY".to_string(),
            price: dec!(5_000_000),
            size: dec!(0.01),
            commission: dec!(0),
            swap_point_accumulate: dec!(0),
            require_collateral: dec!(0),
            open_date: "2025-01-01T00:00:00".to_string(),
            leverage: dec!(2),
            pnl: dec!(0),
            sfd: dec!(0),
        }])
        .build();

    // Test send_child_order
    let resp = mock
        .send_child_order(auto_trader_market::bitflyer_private::SendChildOrderRequest {
            product_code: "FX_BTC_JPY".to_string(),
            child_order_type: auto_trader_market::bitflyer_private::ChildOrderType::Market,
            side: auto_trader_market::bitflyer_private::Side::Buy,
            size: dec!(0.01),
            price: None,
            minute_to_expire: None,
            time_in_force: None,
        })
        .await
        .unwrap();
    assert_eq!(resp.child_order_acceptance_id, "test-123");

    // Test get_positions
    let positions = mock.get_positions("FX_BTC_JPY").await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].product_code, "FX_BTC_JPY");

    // Verify call counters
    assert_eq!(
        mock.counters
            .send_child_order
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        mock.counters
            .get_positions
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn mock_exchange_api_fails_then_succeeds() {
    use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
    use auto_trader_market::exchange_api::ExchangeApi;

    let mock = MockExchangeApiBuilder::new()
        .with_get_positions_response(vec![])
        .with_failures("get_positions", 2)
        .build();

    // First 2 calls fail
    assert!(mock.get_positions("X").await.is_err());
    assert!(mock.get_positions("X").await.is_err());
    // Third succeeds
    assert!(mock.get_positions("X").await.is_ok());
}

// ── MockGmoFxServer ─────────────────────────────────────────────────────────

#[tokio::test]
async fn mock_gmo_fx_server_normal_ticker() {
    use auto_trader_integration_tests::mocks::gmo_fx_server::MockGmoFxServer;

    let mock = MockGmoFxServer::start().await;
    mock.normal_ticker(&["USD_JPY", "EUR_JPY"]).await;

    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .get(format!("{}/public/v1/ticker", mock.url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["status"], 0);
    let data = resp["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    assert_eq!(data[0]["symbol"], "USD_JPY");
    assert_eq!(data[0]["status"], "OPEN");
    assert_eq!(data[1]["symbol"], "EUR_JPY");
}

#[tokio::test]
async fn mock_gmo_fx_server_maintenance() {
    use auto_trader_integration_tests::mocks::gmo_fx_server::MockGmoFxServer;

    let mock = MockGmoFxServer::start().await;
    mock.maintenance().await;

    let resp: serde_json::Value = reqwest::get(format!("{}/public/v1/ticker", mock.url()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["status"], 5);
}

// ── MockSlackWebhook ────────────────────────────────────────────────────────

#[tokio::test]
async fn mock_slack_webhook_captures_body() {
    use auto_trader_integration_tests::mocks::slack_webhook::MockSlackWebhook;

    let (mock, url) = MockSlackWebhook::start().await;

    let client = reqwest::Client::new();
    client
        .post(&url)
        .body(r#"{"text":"hello"}"#)
        .send()
        .await
        .unwrap();

    let bodies = mock.captured_bodies();
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].contains("hello"));
}

#[tokio::test]
async fn mock_slack_webhook_error_response() {
    use auto_trader_integration_tests::mocks::slack_webhook::MockSlackWebhook;

    let (mock, url) = MockSlackWebhook::start().await;
    mock.with_error_response(500).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .body(r#"{"text":"fail"}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 500);
}

// ── MockGemini ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn mock_gemini_parameter_proposal() {
    use auto_trader_integration_tests::mocks::gemini::MockGemini;

    let mock = MockGemini::start().await;
    let proposal_json = r#"{"params":{"entry_channel":20,"exit_channel":10,"atr_baseline_bars":50},"rationale":"test","expected_effect":"none"}"#;
    mock.parameter_proposal(proposal_json).await;

    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(format!(
            "{}/v1beta/models/gemini-flash:generateContent",
            mock.url()
        ))
        .json(&serde_json::json!({"contents": [{"parts": [{"text": "test"}]}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let text = resp["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["params"]["entry_channel"], 20);
    assert_eq!(parsed["rationale"], "test");
}

#[tokio::test]
async fn mock_gemini_invalid_response() {
    use auto_trader_integration_tests::mocks::gemini::MockGemini;

    let mock = MockGemini::start().await;
    mock.invalid_response().await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{}/v1beta/models/gemini-flash:generateContent",
            mock.url()
        ))
        .json(&serde_json::json!({"contents": []}))
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    // The body is intentionally malformed — not parseable as Gemini response
    assert!(serde_json::from_str::<serde_json::Value>(&body).is_err());
}
