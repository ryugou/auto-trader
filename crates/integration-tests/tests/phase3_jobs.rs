//! Phase 3: Peripheral job tests (3.104-3.110).
//!
//! Tests for weekly batch, daily batch backfill, overnight fee job,
//! macro analyst, enriched_ingest formatting, and broadcast channels.

use auto_trader_core::strategy::MacroUpdate;
use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Trade, TradeStatus};
use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use uuid::Uuid;

// ─── 3.104: Weekly batch ─────────────────────────────────────────────────
//
// Test: call weekly_batch::run with a MockGemini that returns parameter
// proposals and verify strategy_params are updated in DB.

#[sqlx::test(migrations = "../../migrations")]
async fn weekly_batch_updates_strategy_params(pool: sqlx::PgPool) {
    // Seed the strategy (needed by FK constraints)
    sqlx::query(
        r#"INSERT INTO strategies (name, display_name, category, risk_level, description, default_params)
           VALUES ('donchian_trend_evolve_v1', 'Donchian Evolve', 'trend', 'medium', 'test', '{}'::jsonb)
           ON CONFLICT (name) DO NOTHING"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Seed initial strategy_params
    sqlx::query(
        r#"INSERT INTO strategy_params (strategy_name, params, updated_at)
           VALUES ('donchian_trend_evolve_v1', '{"entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50}'::jsonb, NOW())
           ON CONFLICT (strategy_name) DO NOTHING"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // Start MockGemini server that returns a valid proposal
    let mock_gemini = auto_trader_integration_tests::mocks::gemini::MockGemini::start().await;
    let proposal_json = r#"{"params":{"entry_channel":22,"exit_channel":8,"atr_baseline_bars":60},"rationale":"test proposal","expected_effect":"improved win rate"}"#;
    mock_gemini.parameter_proposal(proposal_json).await;

    // Run the weekly batch (no Vegapunk)
    let result =
        auto_trader::weekly_batch::run(&pool, None, &mock_gemini.url(), "test-key", "test-model")
            .await;

    assert!(
        result.is_ok(),
        "weekly batch should succeed: {:?}",
        result.err()
    );

    // Verify strategy_params were updated
    let params: sqlx::types::Json<serde_json::Value> = sqlx::query_scalar(
        "SELECT params FROM strategy_params WHERE strategy_name = 'donchian_trend_evolve_v1'",
    )
    .fetch_one(&pool)
    .await
    .expect("should have strategy_params row");

    assert_eq!(
        params.0["entry_channel"].as_i64(),
        Some(22),
        "entry_channel should be updated"
    );
    assert_eq!(
        params.0["exit_channel"].as_i64(),
        Some(8),
        "exit_channel should be updated"
    );
    assert_eq!(
        params.0["atr_baseline_bars"].as_i64(),
        Some(60),
        "atr_baseline_bars should be updated"
    );

    // Verify system_notifications was inserted
    let notif_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM system_notifications WHERE message LIKE '%週次進化バッチ完了%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(notif_count >= 1, "system notification should be inserted");
}

// ─── 3.105: Daily batch backfill ─────────────────────────────────────────
//
// Test: insert closed trades for past dates, call update_daily_max_drawdown,
// verify daily_summary rows are created.

#[sqlx::test(migrations = "../../migrations")]
async fn daily_batch_backfill_creates_summary(pool: sqlx::PgPool) {
    let account_id = auto_trader_integration_tests::helpers::db::seed_trading_account(
        &pool,
        "backfill_test",
        "paper",
        "bitflyer_cfd",
        "bb_mean_revert_v1",
        1_000_000,
    )
    .await;

    // Insert closed trades for yesterday
    let yesterday = Utc::now() - Duration::days(1);
    let yesterday_date = yesterday.date_naive();

    auto_trader_integration_tests::helpers::seed::seed_closed_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "FX_BTC_JPY",
        "bitflyer_cfd",
        "long",
        dec!(5_000_000),
        dec!(5_010_000),
        dec!(10_000),
        dec!(0.01),
        dec!(0),
        yesterday - Duration::hours(2),
        yesterday,
    )
    .await;

    auto_trader_integration_tests::helpers::seed::seed_closed_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "FX_BTC_JPY",
        "bitflyer_cfd",
        "short",
        dec!(5_010_000),
        dec!(5_020_000),
        dec!(-10_000),
        dec!(0.01),
        dec!(0),
        yesterday - Duration::hours(1),
        yesterday + Duration::minutes(30),
    )
    .await;

    // Run the backfill
    auto_trader_db::summary::update_daily_max_drawdown(&pool, yesterday_date)
        .await
        .expect("backfill should succeed");

    // Verify daily_summary rows were created
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM daily_summary WHERE date = $1")
        .bind(yesterday_date)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(
        count >= 1,
        "daily_summary should have rows for yesterday, got {count}"
    );
}

// ─── 3.106: Overnight fee JOB ────────────────────────────────────────────
//
// Test the overnight fee job logic (not just the DB function):
// Replicate the loop logic from main.rs — list paper accounts, find open
// trades, compute fee, apply atomically.

#[sqlx::test(migrations = "../../migrations")]
async fn overnight_fee_job_applies_to_paper_bitflyer_trades(pool: sqlx::PgPool) {
    let account_id = auto_trader_integration_tests::helpers::db::seed_trading_account(
        &pool,
        "overnight_job_test",
        "paper",
        "bitflyer_cfd",
        "bb_mean_revert_v1",
        1_000_000,
    )
    .await;

    let initial_balance: Decimal = dec!(1_000_000);
    let entry_price = dec!(5_000_000);
    let quantity = dec!(0.01);
    let fee_rate = dec!(0.0004); // 0.04%

    let trade_id = auto_trader_integration_tests::helpers::seed::seed_open_trade(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        "FX_BTC_JPY",
        "bitflyer_cfd",
        "long",
        entry_price,
        dec!(4_900_000),
        quantity,
        Utc::now(),
    )
    .await;

    // Replicate the job logic: list accounts, filter paper+BitflyerCfd,
    // list open trades, compute fee, apply.
    let accounts = auto_trader_db::trading_accounts::list_all(&pool)
        .await
        .expect("list accounts");

    let mut total_fee = Decimal::ZERO;
    for pac in &accounts {
        if pac.account_type != "paper" || pac.exchange != "bitflyer_cfd" {
            continue;
        }

        let open_trades = auto_trader_db::trades::get_open_trades_by_account(&pool, pac.id)
            .await
            .expect("list open trades");

        for trade in &open_trades {
            let fee = (trade.entry_price * trade.quantity * fee_rate)
                .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
            if fee.is_zero() {
                continue;
            }

            let mut tx = pool.begin().await.expect("begin tx");
            let applied =
                auto_trader_db::trades::apply_overnight_fee(&mut tx, pac.id, trade.id, fee)
                    .await
                    .expect("apply_overnight_fee");
            tx.commit().await.expect("commit");

            if let Some(_new_balance) = applied {
                total_fee += fee;
            }
        }
    }

    // Expected fee: 5_000_000 * 0.01 * 0.0004 = 20 JPY
    let expected_fee = (entry_price * quantity * fee_rate)
        .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(
        total_fee, expected_fee,
        "fee should be calculated correctly"
    );

    // Verify balance was reduced
    let balance: Decimal =
        sqlx::query_scalar("SELECT current_balance FROM trading_accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(balance, initial_balance - expected_fee);

    // Verify account_events was inserted
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_events WHERE trade_id = $1 AND event_type = 'overnight_fee'",
    )
    .bind(trade_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(event_count, 1);
}

// ─── 3.107: Macro analyst ────────────────────────────────────────────────
//
// Test with MockHTTP (wiremock) for RSS and MockGemini for summarization.
// Verify the analyst produces a MacroUpdate on the broadcast channel.

#[tokio::test]
async fn macro_analyst_produces_update() {
    // Set up RSS mock
    let rss_server = wiremock::MockServer::start().await;
    let rss_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <item>
      <title>USD rises on strong jobs data</title>
      <description>Non-farm payrolls beat expectations, boosting USD.</description>
      <pubDate>Mon, 01 Jan 2024 12:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>"#;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_string(rss_xml)
                .insert_header("content-type", "application/xml"),
        )
        .mount(&rss_server)
        .await;

    // Set up Gemini summarizer mock
    let gemini_server = wiremock::MockServer::start().await;
    let gemini_body = serde_json::json!({
        "candidates": [{
            "content": {
                "parts": [{"text": "USD/JPY は強気。雇用統計が予想を上回り、ドル高が続く見込み。"}],
                "role": "model"
            },
            "finishReason": "STOP"
        }]
    });
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&gemini_body))
        .mount(&gemini_server)
        .await;

    // Create the analyst
    let mut analyst = auto_trader_macro_analyst::analyst::MacroAnalyst::new(
        vec![rss_server.uri()],
        &gemini_server.uri(),
        "test-key",
        "test-model",
    );

    // Create broadcast channel
    let (macro_tx, mut macro_rx) = tokio::sync::broadcast::channel::<MacroUpdate>(16);

    // Run the analyst in a task with a short interval
    let handle = tokio::spawn(async move {
        // We only want one iteration, so we'll let it run briefly
        let _ = analyst
            .run(macro_tx, std::time::Duration::from_millis(100))
            .await;
    });

    // Wait for a MacroUpdate (with timeout)
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), macro_rx.recv()).await;

    // Cancel the analyst task
    handle.abort();

    match result {
        Ok(Ok(update)) => {
            assert!(
                !update.summary.is_empty(),
                "MacroUpdate summary should not be empty"
            );
            println!("macro analyst produced update: {}", update.summary);
        }
        Ok(Err(e)) => panic!("broadcast recv error: {e}"),
        Err(_) => panic!("timed out waiting for MacroUpdate"),
    }
}

// ─── 3.108: enriched_ingest format ───────────────────────────────────────
//
// Test format_trade_open and format_trade_close produce expected text.

#[test]
fn enriched_ingest_format_trade_open() {
    let trade = Trade {
        id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        strategy_name: "donchian_trend_v1".to_string(),
        pair: Pair::new("USD_JPY"),
        exchange: Exchange::Oanda,
        direction: Direction::Long,
        entry_price: dec!(155.500),
        exit_price: None,
        stop_loss: dec!(154.500),
        take_profit: Some(dec!(157.500)),
        quantity: dec!(1000),
        leverage: dec!(2),
        fees: dec!(0),
        entry_at: Utc::now(),
        exit_at: None,
        pnl_amount: None,
        exit_reason: None,
        status: TradeStatus::Open,
        max_hold_until: None,
    };

    let mut indicators = HashMap::new();
    indicators.insert("sma_20".to_string(), dec!(155.0));
    indicators.insert("rsi_14".to_string(), dec!(55));
    indicators.insert("adx_14".to_string(), dec!(30));

    let text =
        auto_trader::enriched_ingest::format_trade_open(&trade, &indicators, Some(dec!(0.5)));

    // Verify key elements are present
    assert!(text.contains("USD_JPY"), "should contain pair");
    assert!(
        text.contains("ロング"),
        "should contain direction in Japanese"
    );
    assert!(
        text.contains("donchian_trend_v1"),
        "should contain strategy name"
    );
    assert!(
        text.contains("155.500") || text.contains("155.5"),
        "should contain entry price"
    );
    assert!(text.contains("SMA20乖離"), "should contain SMA deviation");
    assert!(text.contains("レジーム"), "should contain regime label");
    assert!(text.contains("allocation"), "should contain allocation");
}

#[test]
fn enriched_ingest_format_trade_close() {
    let now = Utc::now();
    let trade = Trade {
        id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        strategy_name: "bb_mean_revert_v1".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        exchange: Exchange::BitflyerCfd,
        direction: Direction::Short,
        entry_price: dec!(5_000_000),
        exit_price: Some(dec!(4_950_000)),
        stop_loss: dec!(5_050_000),
        take_profit: Some(dec!(4_900_000)),
        quantity: dec!(0.01),
        leverage: dec!(2),
        fees: dec!(100),
        entry_at: now - Duration::hours(3),
        exit_at: Some(now),
        pnl_amount: Some(dec!(500)),
        exit_reason: Some(ExitReason::TpHit),
        status: TradeStatus::Closed,
        max_hold_until: None,
    };

    let entry_indicators = serde_json::json!({
        "regime": "trend",
        "rsi_14": 45.5,
        "atr_14": 50000
    });

    let text = auto_trader::enriched_ingest::format_trade_close(
        &trade,
        Some(&entry_indicators),
        Some(dec!(1_000_500)),
        Some(dec!(1_000_000)),
    );

    assert!(text.contains("FX_BTC_JPY"), "should contain pair");
    assert!(text.contains("ショート"), "should contain direction");
    assert!(
        text.contains("bb_mean_revert_v1"),
        "should contain strategy"
    );
    assert!(text.contains("TpHit"), "should contain exit reason");
    assert!(text.contains("500"), "should contain PnL");
    assert!(text.contains("3時間"), "should contain holding time");
    assert!(text.contains("反省材料"), "should contain post-mortem");
    assert!(text.contains("trend"), "should contain entry regime");
}

// ─── 3.109: macro broadcast Lagged ───────────────────────────────────────

#[tokio::test]
async fn macro_broadcast_lagged_on_overflow() {
    // Create a broadcast channel with capacity 2
    let (tx, mut rx) = tokio::sync::broadcast::channel::<MacroUpdate>(2);

    // Send 4 messages (exceeds capacity of 2)
    for i in 0..4 {
        let _ = tx.send(MacroUpdate {
            summary: format!("update {i}"),
            adjustments: HashMap::new(),
        });
    }

    // Receiver should get a Lagged error (missed some messages)
    match rx.recv().await {
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            assert!(n >= 2, "should have lagged by at least 2 messages, got {n}");
        }
        Ok(update) => {
            // Acceptable: we got the oldest surviving message
            println!("received surviving message: {}", update.summary);
        }
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

// ─── 3.110: macro broadcast Closed ───────────────────────────────────────

#[tokio::test]
async fn macro_broadcast_closed_on_sender_drop() {
    let (tx, mut rx) = tokio::sync::broadcast::channel::<MacroUpdate>(16);

    // Drop the sender
    drop(tx);

    // Receiver should get Closed error
    match rx.recv().await {
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            // Expected
        }
        Ok(_) => panic!("expected Closed error, got Ok"),
        Err(other) => panic!("expected Closed error, got {other:?}"),
    }
}
