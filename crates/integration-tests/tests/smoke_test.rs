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

// ── MockVegapunk ──────────────────────────────────────────────────────────

#[tokio::test]
async fn mock_vegapunk_tracks_calls() {
    use auto_trader_integration_tests::mocks::vegapunk::{MockVegapunkBuilder, SearchResult};
    use std::sync::atomic::Ordering;

    let mock = MockVegapunkBuilder::new()
        .with_search_results(vec![SearchResult {
            text: "BTC correlates with risk-off sentiment".to_string(),
            score: 0.92,
        }])
        .build();

    // ingest_raw
    let ingest = mock
        .ingest_raw("test data", "trading_log", "btc", "2026-04-29T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(ingest.chunk_count, 1);

    // search
    let results = mock.search("BTC sentiment", "local", 5).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].score > 0.9);

    // feedback
    mock.feedback("search-001", 4, "helpful").await.unwrap();

    // merge
    mock.merge().await.unwrap();

    // Verify counters
    assert_eq!(mock.counters.ingest_raw.load(Ordering::SeqCst), 1);
    assert_eq!(mock.counters.search.load(Ordering::SeqCst), 1);
    assert_eq!(mock.counters.feedback.load(Ordering::SeqCst), 1);
    assert_eq!(mock.counters.merge.load(Ordering::SeqCst), 1);

    // Verify captured arguments
    let ingest_calls = mock.ingest_raw_calls();
    assert_eq!(ingest_calls.len(), 1);
    assert_eq!(ingest_calls[0].text, "test data");
    assert_eq!(ingest_calls[0].source_type, "trading_log");

    let search_calls = mock.search_calls();
    assert_eq!(search_calls[0].query, "BTC sentiment");
    assert_eq!(search_calls[0].mode, "local");
    assert_eq!(search_calls[0].top_k, 5);

    let feedback_calls = mock.feedback_calls();
    assert_eq!(feedback_calls[0].search_id, "search-001");
    assert_eq!(feedback_calls[0].rating, 4);
}

#[tokio::test]
async fn mock_vegapunk_failure_injection() {
    use auto_trader_integration_tests::mocks::vegapunk::MockVegapunkBuilder;
    use std::sync::atomic::Ordering;

    let mock = MockVegapunkBuilder::new()
        .with_failures("search", 2)
        .build();

    // First 2 calls fail
    assert!(mock.search("q", "local", 3).await.is_err());
    assert!(mock.search("q", "local", 3).await.is_err());
    // Third succeeds
    assert!(mock.search("q", "local", 3).await.is_ok());

    // Counter tracks all calls (including failures)
    assert_eq!(mock.counters.search.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn mock_vegapunk_global_fail_switch() {
    use auto_trader_integration_tests::mocks::vegapunk::MockVegapunk;
    use std::sync::atomic::Ordering;

    let mock = MockVegapunk::new();
    mock.should_fail.store(true, Ordering::SeqCst);

    assert!(mock.ingest_raw("x", "y", "z", "t").await.is_err());
    assert!(mock.search("q", "local", 1).await.is_err());
    assert!(mock.feedback("id", 1, "bad").await.is_err());
    assert!(mock.merge().await.is_err());
}

// ── MockBitflyerWs ─────────────────────────────────────────────────────────

#[tokio::test]
async fn mock_bitflyer_ws_sends_ticks() {
    use auto_trader_integration_tests::mocks::bitflyer_ws::{MockBitflyerWs, TickData};
    use futures_util::StreamExt;

    let ticks = vec![
        TickData::new(11_000_000, 10_999_000, 11_001_000),
        TickData::new(11_050_000, 11_049_000, 11_051_000)
            .with_timestamp("2026-04-28T00:00:01.000"),
        TickData::new(11_100_000, 11_099_000, 11_101_000)
            .with_timestamp("2026-04-28T00:00:02.000"),
    ];

    let mock = MockBitflyerWs::normal_ticks("FX_BTC_JPY", ticks).await;

    let (ws, _) = tokio_tungstenite::connect_async(mock.url())
        .await
        .expect("failed to connect to mock ws");
    let (_write, mut read) = ws.split();

    let mut received = Vec::new();
    for _ in 0..3 {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
            .await
            .expect("timeout waiting for ws message")
            .expect("stream ended unexpectedly")
            .expect("ws error");

        if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed["jsonrpc"], "2.0");
            assert_eq!(parsed["method"], "channelMessage");
            assert_eq!(
                parsed["params"]["channel"],
                "lightning_ticker_FX_BTC_JPY"
            );
            let ltp = parsed["params"]["message"]["ltp"].as_i64().unwrap();
            received.push(ltp);
        }
    }

    assert_eq!(received, vec![11_000_000, 11_050_000, 11_100_000]);
}

#[tokio::test]
async fn mock_bitflyer_ws_disconnect_after() {
    use auto_trader_integration_tests::mocks::bitflyer_ws::{MockBitflyerWs, TickData};
    use futures_util::StreamExt;

    let ticks = vec![
        TickData::new(11_000_000, 10_999_000, 11_001_000),
        TickData::new(11_050_000, 11_049_000, 11_051_000),
        TickData::new(11_100_000, 11_099_000, 11_101_000),
    ];

    let mock = MockBitflyerWs::disconnect_after("FX_BTC_JPY", ticks, 2).await;

    let (ws, _) = tokio_tungstenite::connect_async(mock.url())
        .await
        .expect("failed to connect");
    let (_write, mut read) = ws.split();

    let mut count = 0;
    while let Some(result) =
        tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
            .await
            .ok()
            .flatten()
    {
        match result {
            Ok(tokio_tungstenite::tungstenite::Message::Text(_)) => count += 1,
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
            Err(_) => break,
            _ => {}
        }
    }

    assert_eq!(count, 2, "should receive exactly 2 ticks before disconnect");
}

#[tokio::test]
async fn mock_bitflyer_ws_invalid_message() {
    use auto_trader_integration_tests::mocks::bitflyer_ws::MockBitflyerWs;
    use futures_util::StreamExt;

    let mock = MockBitflyerWs::invalid_message().await;

    let (ws, _) = tokio_tungstenite::connect_async(mock.url())
        .await
        .expect("failed to connect");
    let (_write, mut read) = ws.split();

    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), read.next())
        .await
        .expect("timeout")
        .expect("stream ended")
        .expect("ws error");

    if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
        // Must not be valid JSON-RPC ticker
        let parsed = serde_json::from_str::<serde_json::Value>(&text);
        assert!(
            parsed.is_err(),
            "invalid_message scenario should send non-JSON data"
        );
    }
}

// ── Full Integration Smoke Test ────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn full_integration_smoke_test(pool: sqlx::PgPool) {
    use auto_trader_integration_tests::helpers::failure_output::{self, TracingCapture};
    use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
    use auto_trader_integration_tests::mocks::gemini::MockGemini;
    use auto_trader_integration_tests::mocks::gmo_fx_server::MockGmoFxServer;
    use auto_trader_integration_tests::mocks::slack_webhook::MockSlackWebhook;
    use auto_trader_integration_tests::mocks::vegapunk::MockVegapunkBuilder;
    use auto_trader_market::exchange_api::ExchangeApi;
    use std::path::PathBuf;
    use tracing_subscriber::prelude::*;

    // 1. Seed accounts
    let accounts = db::seed_standard_accounts(&pool).await;
    assert_ne!(
        accounts.bitflyer_cfd_account_id,
        accounts.gmo_fx_account_id
    );

    // 2. Load fixtures
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/smoke_test.csv");
    let count =
        fixture_loader::load_price_candles(&pool, "gmo_fx", "USD_JPY", "M5", &fixture_path)
            .await
            .unwrap();
    assert_eq!(count, 3);

    // 3. Verify DB state via snapshot
    let snapshot =
        db::snapshot_tables(&pool, &["trading_accounts", "price_candles"]).await;
    assert!(
        snapshot.contains("test_bitflyer_cfd"),
        "snapshot must contain bitflyer account: {snapshot}"
    );
    assert!(
        snapshot.contains("test_gmo_fx"),
        "snapshot must contain gmo_fx account: {snapshot}"
    );
    assert!(
        snapshot.contains("USD_JPY"),
        "snapshot must contain USD_JPY candles: {snapshot}"
    );

    // 4. Create and use MockExchangeApi
    let mock_exchange = MockExchangeApiBuilder::new()
        .with_get_positions_response(vec![])
        .build();
    let positions = mock_exchange.get_positions("FX_BTC_JPY").await.unwrap();
    assert!(positions.is_empty());

    // 5. Start and use MockGmoFxServer
    let mock_gmo = MockGmoFxServer::start().await;
    mock_gmo.normal_ticker(&["USD_JPY"]).await;
    let gmo_resp = reqwest::get(format!("{}/public/v1/ticker", mock_gmo.url()))
        .await
        .unwrap();
    assert_eq!(gmo_resp.status(), 200);

    // 6. Start and use MockSlackWebhook
    let (mock_slack, slack_url) = MockSlackWebhook::start().await;
    let client = reqwest::Client::new();
    client
        .post(&slack_url)
        .body(r#"{"text":"smoke"}"#)
        .send()
        .await
        .unwrap();
    let bodies = mock_slack.captured_bodies();
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].contains("smoke"));

    // 7. Start and use MockGemini
    let mock_gemini = MockGemini::start().await;
    mock_gemini
        .parameter_proposal(r#"{"entry_channel":20,"exit_channel":10}"#)
        .await;
    let gemini_resp: serde_json::Value = client
        .post(format!(
            "{}/v1beta/models/gemini-flash:generateContent",
            mock_gemini.url()
        ))
        .json(&serde_json::json!({"contents": [{"parts": [{"text": "test"}]}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = gemini_resp["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["entry_channel"], 20);

    // 8. Create and use MockVegapunk
    let mock_vegapunk = MockVegapunkBuilder::new().build();
    let ingest = mock_vegapunk
        .ingest_raw("test data", "trading_log", "btc", "2026-04-29T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(ingest.chunk_count, 1);

    // 9. Verify tracing capture
    let (layer, buffer) = TracingCapture::new();
    let _guard = tracing_subscriber::registry().with(layer).set_default();
    tracing::info!("smoke test complete");
    let logs = buffer.lock().unwrap();
    assert!(
        logs.iter().any(|l| l.contains("smoke test complete")),
        "tracing capture must record log line"
    );
    drop(logs);

    // 10. Verify failure output format
    let logs_snapshot = buffer.lock().unwrap();
    let report = failure_output::format_failure(
        "smoke_test::full_integration",
        "fixtures/smoke_test.csv",
        "everything works",
        "it did",
        &logs_snapshot,
        &snapshot,
    );
    drop(logs_snapshot);

    assert!(report.contains("[FAIL]"));
    assert!(report.contains("smoke_test::full_integration"));
    assert!(report.contains("=== application log ==="));
    assert!(report.contains("=== db state ==="));
    assert!(report.contains("smoke test complete"));
}
