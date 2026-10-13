use uuid::Uuid;

use super::*;

#[test]
fn extract_json_plain_passes_through() {
    let input = r#"{"params":{},"rationale":"test","expected_effect":""}"#;
    assert_eq!(extract_json(input), input);
}

#[test]
fn extract_json_strips_code_fence() {
    let input = "```json\n{\"params\":{},\"rationale\":\"r\",\"expected_effect\":\"\"}\n```";
    let extracted = extract_json(input);
    assert!(extracted.starts_with('{'));
    assert!(extracted.ends_with('}'));
}

#[test]
fn extract_json_strips_plain_fence() {
    let input = "```\n{\"params\":{},\"rationale\":\"r\",\"expected_effect\":\"\"}\n```";
    let extracted = extract_json(input);
    assert!(extracted.starts_with('{'));
}

#[test]
fn build_gemini_prompt_contains_key_sections() {
    let stats = StrategyStats {
        total_trades: 42,
        wins: 28,
        avg_pnl: 150.5,
    };
    let wilson = vec![RegimeAnalysis {
        regime: "trending".to_string(),
        trades: 20,
        wins: 15,
        wilson_lb: 0.55,
    }];
    let params = serde_json::json!({"entry_channel": 20});

    let prompt = build_gemini_prompt(
        "donchian_trend_evolve_v1",
        &stats,
        Some("vp context text"),
        &params,
        &wilson,
    );

    assert!(prompt.contains("42"));
    assert!(prompt.contains("donchian_trend_evolve_v1"));
    assert!(prompt.contains("trending"));
    assert!(prompt.contains("Wilson"));
    assert!(prompt.contains("vp context text"));
    assert!(prompt.contains("entry_channel"));
}

#[test]
fn build_gemini_prompt_no_knowledge_context() {
    let stats = StrategyStats {
        total_trades: 0,
        wins: 0,
        avg_pnl: 0.0,
    };
    let wilson: Vec<RegimeAnalysis> = vec![];
    let params = serde_json::json!({});

    let prompt = build_gemini_prompt("donchian_trend_evolve_v1", &stats, None, &params, &wilson);

    assert!(!prompt.contains("過去トレード学習コンテキスト"));
    assert!(prompt.contains("rationale"));
}

#[test]
fn build_gemini_prompt_uses_strategy_specific_instructions() {
    let stats = StrategyStats {
        total_trades: 27,
        wins: 10,
        avg_pnl: -140.7,
    };
    let prompt = build_gemini_prompt(
        "bb_mean_revert_v1",
        &stats,
        None,
        &serde_json::json!({"something": 1}),
        &[],
    );

    assert!(prompt.contains("bb_mean_revert_v1"));
    assert!(!prompt.contains("entry_channel"));
    assert!(prompt.contains("現在のパラメータ構造を維持"));
}

#[test]
fn validate_params_donchian_normalizes_and_drops_extras() {
    let proposed = serde_json::json!({
        "entry_channel": 20,
        "exit_channel": 10,
        "atr_baseline_bars": 50,
        "sl_pct": 0.03,
    });
    let current = serde_json::json!({});
    let v = validate_params("donchian_trend_evolve_v1", &proposed, &current).unwrap();
    assert_eq!(v["entry_channel"], 20);
    assert_eq!(v["exit_channel"], 10);
    assert_eq!(v["atr_baseline_bars"], 50);
    assert!(v.get("sl_pct").is_none(), "extra keys should be dropped");
}

#[test]
fn validate_params_donchian_rejects_out_of_range() {
    let proposed = serde_json::json!({
        "entry_channel": 100,
        "exit_channel": 10,
        "atr_baseline_bars": 50,
    });
    let current = serde_json::json!({});
    let err = validate_params("donchian_trend_evolve_v1", &proposed, &current).unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid entry_channel: out of range [10, 30]"
    );
}

#[test]
fn validate_params_donchian_rejects_exit_ge_entry() {
    let proposed = serde_json::json!({
        "entry_channel": 12,
        "exit_channel": 15,
        "atr_baseline_bars": 50,
    });
    let current = serde_json::json!({});
    let err = validate_params("donchian_trend_evolve_v1", &proposed, &current).unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid exit_channel: must be less than entry_channel"
    );
}

#[test]
fn validate_params_donchian_rejects_missing_key() {
    let proposed = serde_json::json!({
        "exit_channel": 10,
        "atr_baseline_bars": 50,
    });
    let current = serde_json::json!({});
    let err = validate_params("donchian_trend_evolve_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("entry_channel"),
        "error should name the missing key: {err}"
    );
}

#[test]
fn validate_params_permissive_accepts_matching_key_set() {
    let proposed = serde_json::json!({ "window": 20, "threshold": 1.5 });
    let current = serde_json::json!({ "window": 15, "threshold": 1.0 });
    let v = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap();
    assert_eq!(v["window"], 20);
}

#[test]
fn validate_params_permissive_rejects_empty_object() {
    let proposed = serde_json::json!({});
    let current = serde_json::json!({ "window": 15 });
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_err());
}

#[test]
fn validate_params_permissive_rejects_non_object() {
    let proposed = serde_json::json!([1, 2, 3]);
    let current = serde_json::json!({ "window": 15 });
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_err());
}

#[test]
fn validate_params_permissive_rejects_null_values() {
    let proposed = serde_json::json!({"window": null});
    let current = serde_json::json!({ "window": 15 });
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_err());
}

#[test]
fn validate_params_permissive_rejects_added_key() {
    let proposed = serde_json::json!({ "window": 20, "threshold": 1.5, "rogue": 99 });
    let current = serde_json::json!({ "window": 15, "threshold": 1.0 });
    let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("rogue"),
        "error should name the extra key: {err}"
    );
}

#[test]
fn validate_params_permissive_rejects_dropped_key() {
    let proposed = serde_json::json!({ "window": 20 });
    let current = serde_json::json!({ "window": 15, "threshold": 1.0 });
    let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("threshold"),
        "error should name the dropped key: {err}"
    );
}

#[test]
fn validate_params_permissive_rejects_type_swap() {
    let proposed = serde_json::json!({ "window": "20" });
    let current = serde_json::json!({ "window": 15 });
    let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("type mismatch"),
        "error should flag type mismatch: {err}"
    );
}

#[test]
fn validate_params_permissive_rejects_int_to_float_swap() {
    let proposed = serde_json::json!({ "window": 20.5 });
    let current = serde_json::json!({ "window": 20 });
    let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("type mismatch"),
        "int→float swap should be rejected: {err}"
    );
}

#[test]
fn validate_params_permissive_accepts_float_to_float() {
    let proposed = serde_json::json!({ "threshold": 1.6 });
    let current = serde_json::json!({ "threshold": 1.2 });
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_ok());
}

#[test]
fn validate_params_permissive_relaxes_when_current_empty() {
    let proposed = serde_json::json!({ "anything": 1 });
    let current = serde_json::json!({});
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_ok());
}

#[tokio::test]
async fn call_gemini_rejects_empty_api_key() {
    let err = call_gemini("http://unused.invalid", "", "model", "prompt")
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("api_key"),
        "error should mention api_key: {err}"
    );
}

async fn seed_paper_account(pool: &PgPool, strategy_name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO trading_accounts
               (id, name, account_type, exchange, strategy,
                initial_balance, current_balance, leverage, currency)
           VALUES ($1, 'weekly_batch_test', 'paper', 'bitflyer_cfd', $2,
                   30000, 30000, 2, 'JPY')"#,
    )
    .bind(id)
    .bind(strategy_name)
    .execute(pool)
    .await
    .expect("seed_paper_account failed");
    id
}

fn gemini_url_refused_without_reaching_the_network() -> &'static str {
    "http://127.0.0.1:1"
}

async fn seed_closed_trades(pool: &PgPool, account_id: Uuid, strategy_name: &str, count: i64) {
    for _ in 0..count {
        sqlx::query(
            r#"INSERT INTO trades
                   (id, account_id, strategy_name, pair, exchange, direction,
                    entry_price, entry_at, stop_loss, quantity, leverage, status,
                    exit_price, exit_at, pnl_amount, fees, exit_reason)
               VALUES ($1, $2, $3, 'FX_BTC_JPY', 'bitflyer_cfd', 'long',
                       5000000, NOW() - INTERVAL '2 days', 4950000, 0.01, 2, 'closed',
                       5010000, NOW() - INTERVAL '1 day', 100, 0, 'tp_hit')"#,
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(strategy_name)
        .execute(pool)
        .await
        .expect("seed_closed_trades failed");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_for_strategy_skips_evaluation_below_min_trades_for_evolution(pool: PgPool) {
    let initial_params = serde_json::json!({"window": 20});
    persist_params(&pool, "bb_mean_revert_v1", &initial_params)
        .await
        .expect("persist_params failed");

    let account_id = seed_paper_account(&pool, "bb_mean_revert_v1").await;
    seed_closed_trades(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        MIN_TRADES_FOR_EVOLUTION - 1,
    )
    .await;

    let result = run_for_strategy(
        &pool,
        None,
        "not-a-url",
        "unused-key",
        "unused-model",
        "bb_mean_revert_v1",
    )
    .await;
    assert!(
        result.is_ok(),
        "evaluation below MIN_TRADES_FOR_EVOLUTION must be skipped, not errored: {:?}",
        result.err()
    );

    let params_after = load_current_params(&pool, "bb_mean_revert_v1")
        .await
        .expect("load_current_params failed");
    assert_eq!(
        params_after, initial_params,
        "params must be kept unchanged when the trade count is below MIN_TRADES_FOR_EVOLUTION"
    );

    let notif_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM system_notifications WHERE message LIKE '%サンプル不足%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        notif_count >= 1,
        "a sample-insufficient notification must be recorded when evaluation is skipped"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn gemini_failure_records_system_notification_and_keeps_params(pool: PgPool) {
    let initial_params = serde_json::json!({"window": 20});
    persist_params(&pool, "bb_mean_revert_v1", &initial_params)
        .await
        .expect("persist_params failed");

    let account_id = seed_paper_account(&pool, "bb_mean_revert_v1").await;
    seed_closed_trades(
        &pool,
        account_id,
        "bb_mean_revert_v1",
        MIN_TRADES_FOR_EVOLUTION,
    )
    .await;

    let result = run_for_strategy(
        &pool,
        None,
        gemini_url_refused_without_reaching_the_network(),
        "unused-key",
        "unused-model",
        "bb_mean_revert_v1",
    )
    .await;
    assert!(
        result.is_ok(),
        "a gemini call failure must not propagate as an error: {:?}",
        result.err()
    );

    let params_after = load_current_params(&pool, "bb_mean_revert_v1")
        .await
        .expect("load_current_params failed");
    assert_eq!(
        params_after, initial_params,
        "params must be kept unchanged when the gemini call fails"
    );

    let notif_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM system_notifications WHERE message LIKE '%LLM 呼び出し失敗%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        notif_count >= 1,
        "a gemini call failure must be recorded in system_notifications"
    );
}

async fn spawn_gemini_server_proposing(
    proposal_json: &str,
) -> (String, tokio::task::JoinHandle<()>) {
    let body = serde_json::json!({
        "candidates": [{"content": {"parts": [{"text": proposal_json}]}}]
    });
    let router = axum::Router::new().fallback(move || {
        let body = body.clone();
        async move { axum::Json(body) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind gemini mock server");
    let addr = listener.local_addr().expect("local_addr failed");
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("gemini mock server stopped with an error");
    });
    (format!("http://{addr}"), server)
}

#[sqlx::test(migrations = "../../migrations")]
async fn validation_failure_records_rejected_params_in_system_notification(pool: PgPool) {
    let strategy = "donchian_trend_evolve_v1";
    let initial_params =
        serde_json::json!({"entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50});
    persist_params(&pool, strategy, &initial_params)
        .await
        .expect("persist_params failed");

    let account_id = seed_paper_account(&pool, strategy).await;
    seed_closed_trades(&pool, account_id, strategy, MIN_TRADES_FOR_EVOLUTION).await;

    let proposal = r#"{"params":{"entry_channel":100,"exit_channel":10,"atr_baseline_bars":50},"rationale":"r","expected_effect":"e"}"#;
    let (api_url, server) = spawn_gemini_server_proposing(proposal).await;

    let result = run_for_strategy(&pool, None, &api_url, "test-key", "test-model", strategy).await;
    assert!(
        result.is_ok(),
        "a validation failure must not propagate as an error: {:?}",
        result.err()
    );

    let params_after = load_current_params(&pool, strategy)
        .await
        .expect("load_current_params failed");
    assert_eq!(
        params_after, initial_params,
        "params must be kept unchanged when validation rejects the proposal"
    );

    let message: String = sqlx::query_scalar(
        "SELECT message FROM system_notifications WHERE message LIKE '%検証に失敗%'",
    )
    .fetch_one(&pool)
    .await
    .expect("validation failure notification not found");
    assert!(
        message.contains("invalid entry_channel: out of range [10, 30]"),
        "notification must carry the rejection reason: {message}"
    );
    assert!(
        message.contains(r#""entry_channel":100"#),
        "notification must carry the rejected proposal params: {message}"
    );

    assert!(
        !server.is_finished(),
        "gemini mock server must still be serving"
    );
    server.abort();
    let result = server.await;
    assert!(result.as_ref().is_err_and(|e| e.is_cancelled()));
}
