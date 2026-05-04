//! Phase 1: Warmup, strategy registration, notification purge tests.

use auto_trader_core::config::{GeminiConfig, StrategyConfig};
use auto_trader_core::event::SignalEvent;
use auto_trader_integration_tests::helpers::{db, seed};
use auto_trader_strategy::engine::StrategyEngine;
use chrono::{TimeZone, Utc};
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tokio::sync::mpsc;

// ── Strategy Registration ────────────────────────────────────────────────

/// All 4 standard strategies (excluding swing_llm) register successfully when enabled.
#[sqlx::test(migrations = "../../migrations")]
async fn register_all_standard_strategies(pool: sqlx::PgPool) {
    let (signal_tx, _signal_rx) = mpsc::channel::<SignalEvent>(16);
    let mut engine = StrategyEngine::new(signal_tx);
    let strategies = vec![
        strategy_cfg("bb_mean_revert_v1", true, &["USD_JPY"]),
        strategy_cfg("donchian_trend_v1", true, &["USD_JPY"]),
        strategy_cfg("donchian_trend_evolve_v1", true, &["USD_JPY"]),
        strategy_cfg("squeeze_momentum_v1", true, &["USD_JPY"]),
        // swing_llm requires GEMINI_API_KEY + vegapunk — skip in this test
    ];

    auto_trader::startup::register_strategies(
        &mut engine,
        &strategies,
        &pool,
        &None, // no vegapunk
        "test-schema",
        None, // no gemini config
    )
    .await;

    // 4 strategies registered (swing_llm excluded from input).
    assert_eq!(engine.registered_names().len(), 4);
}

/// Disabled strategies are skipped.
#[sqlx::test(migrations = "../../migrations")]
async fn register_disabled_strategies_skipped(pool: sqlx::PgPool) {
    let (signal_tx, _signal_rx) = mpsc::channel::<SignalEvent>(16);
    let mut engine = StrategyEngine::new(signal_tx);
    let strategies = vec![
        strategy_cfg("bb_mean_revert_v1", false, &["USD_JPY"]),
        strategy_cfg("donchian_trend_v1", false, &["USD_JPY"]),
    ];

    auto_trader::startup::register_strategies(
        &mut engine, &strategies, &pool, &None, "test-schema", None,
    )
    .await;

    assert_eq!(engine.registered_names().len(), 0);
}

/// Unknown strategy names are skipped with a warning (no panic).
#[sqlx::test(migrations = "../../migrations")]
async fn register_unknown_strategy_skipped(pool: sqlx::PgPool) {
    let (signal_tx, _signal_rx) = mpsc::channel::<SignalEvent>(16);
    let mut engine = StrategyEngine::new(signal_tx);
    let strategies = vec![strategy_cfg("totally_unknown_v99", true, &["USD_JPY"])];

    auto_trader::startup::register_strategies(
        &mut engine, &strategies, &pool, &None, "test-schema", None,
    )
    .await;

    assert_eq!(engine.registered_names().len(), 0);
}

/// swing_llm is skipped when GEMINI_API_KEY is not set.
#[sqlx::test(migrations = "../../migrations")]
async fn register_swing_llm_skipped_without_gemini_key(pool: sqlx::PgPool) {
    // Ensure GEMINI_API_KEY is not set for this test.
    unsafe {
        std::env::remove_var("GEMINI_API_KEY");
    }

    let (signal_tx, _signal_rx) = mpsc::channel::<SignalEvent>(16);
    let mut engine = StrategyEngine::new(signal_tx);
    let strategies = vec![swing_llm_cfg()];
    let gemini = GeminiConfig {
        model: "gemini-2.5-flash".to_string(),
        api_url: "https://generativelanguage.googleapis.com".to_string(),
    };

    auto_trader::startup::register_strategies(
        &mut engine,
        &strategies,
        &pool,
        &None,
        "test-schema",
        Some(&gemini),
    )
    .await;

    assert_eq!(engine.registered_names().len(), 0);
}

/// swing_llm is skipped when vegapunk client is None.
#[sqlx::test(migrations = "../../migrations")]
async fn register_swing_llm_skipped_without_vegapunk(pool: sqlx::PgPool) {
    // Even with GEMINI_API_KEY set, vegapunk=None → skip.
    unsafe {
        std::env::set_var("GEMINI_API_KEY", "test-key-for-integration-test");
    }

    let (signal_tx, _signal_rx) = mpsc::channel::<SignalEvent>(16);
    let mut engine = StrategyEngine::new(signal_tx);
    let strategies = vec![swing_llm_cfg()];
    let gemini = GeminiConfig {
        model: "gemini-2.5-flash".to_string(),
        api_url: "https://generativelanguage.googleapis.com".to_string(),
    };

    auto_trader::startup::register_strategies(
        &mut engine,
        &strategies,
        &pool,
        &None,
        "test-schema",
        Some(&gemini),
    )
    .await;

    assert_eq!(engine.registered_names().len(), 0);

    // Clean up env var.
    unsafe {
        std::env::remove_var("GEMINI_API_KEY");
    }
}

/// donchian_trend_evolve falls back to defaults when strategy_params query
/// fails (e.g., table exists but no row for this strategy).
#[sqlx::test(migrations = "../../migrations")]
async fn register_donchian_evolve_fallback_on_missing_params(pool: sqlx::PgPool) {
    let (signal_tx, _signal_rx) = mpsc::channel::<SignalEvent>(16);
    let mut engine = StrategyEngine::new(signal_tx);
    let strategies = vec![strategy_cfg(
        "donchian_trend_evolve_v1",
        true,
        &["USD_JPY"],
    )];

    // No strategy_params row inserted — should fallback to defaults.
    auto_trader::startup::register_strategies(
        &mut engine, &strategies, &pool, &None, "test-schema", None,
    )
    .await;

    assert_eq!(engine.registered_names().len(), 1);
}

// ── Notification Purge ───────────────────────────────────────────────────

/// purge_old_read deletes read notifications older than 30 days.
#[sqlx::test(migrations = "../../migrations")]
async fn notification_purge_deletes_old_read(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool,
        "purge_test",
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
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    )
    .await;

    // Old read notification (40 days ago).
    let old_read_at = Utc::now() - chrono::Duration::days(40);
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
        Some(old_read_at),
    )
    .await;

    // Recent read notification (5 days ago).
    let recent_read_at = Utc::now() - chrono::Duration::days(5);
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
        Some(recent_read_at),
    )
    .await;

    // Unread notification.
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

    let purged = auto_trader_db::notifications::purge_old_read(&pool)
        .await
        .unwrap();
    assert_eq!(purged, 1, "should purge only the old read notification");

    let (_remaining, total) =
        auto_trader_db::notifications::list(&pool, 100, 0, false, None, None, None)
            .await
            .unwrap();
    assert_eq!(total, 2, "2 notifications should remain");
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn strategy_cfg(name: &str, enabled: bool, pairs: &[&str]) -> StrategyConfig {
    StrategyConfig {
        name: name.to_string(),
        enabled,
        mode: "paper".to_string(),
        pairs: pairs.iter().map(|s| s.to_string()).collect(),
        params: HashMap::new(),
    }
}

fn swing_llm_cfg() -> StrategyConfig {
    let mut params = HashMap::new();
    params.insert("holding_days_max".to_string(), toml::Value::Integer(14));
    StrategyConfig {
        name: "swing_llm_v1".to_string(),
        enabled: true,
        mode: "paper".to_string(),
        pairs: vec!["USD_JPY".to_string()],
        params,
    }
}
