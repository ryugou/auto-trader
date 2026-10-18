#![doc = "Phase 3: weekly batch tests (3.104)."]

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const INITIAL_PARAMS: &str =
    r#"{"entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50}"#;

async fn seed_donchian_evolve_strategy(pool: &PgPool, params_json: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO strategies (name, display_name, category, risk_level, description, default_params)
           VALUES ('donchian_trend_evolve_v1', 'Donchian Evolve', 'trend', 'medium', 'test', '{}'::jsonb)
           ON CONFLICT (name) DO NOTHING"#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO strategy_params (strategy_name, params, updated_at)
           VALUES ('donchian_trend_evolve_v1', $1::jsonb, NOW())
           ON CONFLICT (strategy_name)
           DO UPDATE SET params = EXCLUDED.params, updated_at = NOW()"#,
    )
    .bind(params_json)
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_closed_trades(pool: &PgPool, account_name: &str, pnls: &[f64]) -> anyhow::Result<()> {
    let account_id = auto_trader_integration_tests::helpers::db::seed_trading_account(
        pool,
        account_name,
        "paper",
        "gmo_fx",
        "donchian_trend_evolve_v1",
        1_000_000,
    )
    .await?;
    for pnl in pnls {
        sqlx::query(
            r#"INSERT INTO trades
                   (id, account_id, strategy_name, pair, exchange, direction,
                    entry_price, entry_at, stop_loss, quantity, leverage, status,
                    exit_price, exit_at, pnl_amount, fees, exit_reason)
               VALUES ($1, $2, 'donchian_trend_evolve_v1', 'USD_JPY', 'gmo_fx', 'long',
                       150.0, NOW() - INTERVAL '2 days', 149.0, 1000, 2, 'closed',
                       151.0, NOW() - INTERVAL '1 day', $3, 0, 'tp_hit')"#,
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(*pnl)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn run_weekly_batch_with_proposal(pool: &PgPool, proposal_json: &str) -> anyhow::Result<()> {
    let mock_gemini = auto_trader_integration_tests::mocks::gemini::MockGemini::start().await;
    mock_gemini.parameter_proposal(proposal_json).await;
    auto_trader::weekly_batch::run(pool, None, &mock_gemini.url(), "test-key", "test-model").await
}

async fn fetch_strategy_params(pool: &PgPool) -> anyhow::Result<serde_json::Value> {
    let params: sqlx::types::Json<serde_json::Value> = sqlx::query_scalar(
        "SELECT params FROM strategy_params WHERE strategy_name = 'donchian_trend_evolve_v1'",
    )
    .fetch_one(pool)
    .await?;
    Ok(params.0)
}

async fn count_notifications_like(pool: &PgPool, pattern: &str) -> anyhow::Result<i64> {
    let count =
        sqlx::query_scalar("SELECT COUNT(*) FROM system_notifications WHERE message LIKE $1")
            .bind(pattern)
            .fetch_one(pool)
            .await?;
    Ok(count)
}

async fn assert_proposal_recorded_without_applying(pool: &PgPool) -> anyhow::Result<()> {
    assert_eq!(
        fetch_strategy_params(pool).await?,
        json!({"entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50}),
        "strategy_params must not be changed by the weekly batch"
    );

    let rows: Vec<(serde_json::Value, serde_json::Value, Option<String>, String)> = sqlx::query_as(
        "SELECT base_params, normalized_params, validation_error, llm_model
         FROM strategy_param_proposals WHERE strategy_name = 'donchian_trend_evolve_v1'",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(rows.len(), 1, "exactly one proposal row must be recorded");
    let (base, normalized, validation_error, llm_model) = &rows[0];
    assert_eq!(
        base,
        &json!({"entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50})
    );
    assert_eq!(
        normalized,
        &json!({"entry_channel": 22, "exit_channel": 8, "atr_baseline_bars": 60})
    );
    assert!(validation_error.is_none());
    assert_eq!(llm_model, "test-model");

    let notif_count = count_notifications_like(pool, "%未反映%").await?;
    assert!(notif_count >= 1, "system notification should be inserted");
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn weekly_batch_records_proposal_without_applying(pool: PgPool) {
    seed_donchian_evolve_strategy(&pool, INITIAL_PARAMS)
        .await
        .unwrap();
    seed_closed_trades(
        &pool,
        "weekly_batch_test",
        &[100.0, -50.0, 100.0, -50.0, 100.0, -50.0],
    )
    .await
    .unwrap();

    let proposal_json = r#"{"params":{"entry_channel":22,"exit_channel":8,"atr_baseline_bars":60},"rationale":"test proposal","expected_effect":"improved win rate"}"#;
    let result = run_weekly_batch_with_proposal(&pool, proposal_json).await;

    assert!(
        result.is_ok(),
        "weekly batch should succeed: {:?}",
        result.err()
    );
    assert_proposal_recorded_without_applying(&pool)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn strategy_param_proposals_rejects_update_and_delete(pool: PgPool) {
    sqlx::query(
        r#"INSERT INTO strategy_param_proposals
               (id, strategy_name, base_params, proposed_params, normalized_params,
                rationale, expected_effect, evidence, llm_model)
           VALUES ($1, 's', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, 'r', 'e', '{}'::jsonb, 'm')"#,
    )
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();

    let update = sqlx::query("UPDATE strategy_param_proposals SET rationale = 'x'")
        .execute(&pool)
        .await;
    assert!(update.is_err(), "UPDATE must be rejected");
    let delete = sqlx::query("DELETE FROM strategy_param_proposals")
        .execute(&pool)
        .await;
    assert!(delete.is_err(), "DELETE must be rejected");
}

#[sqlx::test(migrations = "../../migrations")]
async fn weekly_batch_skips_low_sample_strategy(pool: PgPool) {
    seed_donchian_evolve_strategy(&pool, INITIAL_PARAMS)
        .await
        .unwrap();
    seed_closed_trades(&pool, "small_sample_test", &[100.0, 100.0])
        .await
        .unwrap();

    let aggressive_proposal = r#"{"params":{"entry_channel":12,"exit_channel":11,"atr_baseline_bars":20},"rationale":"N=1 win → aggressive","expected_effect":"more trades"}"#;
    let result = run_weekly_batch_with_proposal(&pool, aggressive_proposal).await;
    assert!(
        result.is_ok(),
        "weekly batch should succeed: {:?}",
        result.err()
    );

    let params = fetch_strategy_params(&pool).await.unwrap();
    assert_eq!(
        params["entry_channel"].as_i64(),
        Some(20),
        "entry_channel should be unchanged"
    );
    assert_eq!(
        params["exit_channel"].as_i64(),
        Some(10),
        "exit_channel should be unchanged"
    );
    assert_eq!(
        params["atr_baseline_bars"].as_i64(),
        Some(50),
        "atr_baseline_bars should be unchanged"
    );

    let notif_count = count_notifications_like(&pool, "%サンプル不足%")
        .await
        .unwrap();
    assert!(
        notif_count >= 1,
        "small-sample notification should be inserted"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn weekly_batch_handles_zero_trades(pool: PgPool) {
    seed_donchian_evolve_strategy(&pool, INITIAL_PARAMS)
        .await
        .unwrap();

    let aggressive = r#"{"params":{"entry_channel":12,"exit_channel":11,"atr_baseline_bars":20},"rationale":"x","expected_effect":"y"}"#;
    let result = run_weekly_batch_with_proposal(&pool, aggressive).await;
    assert!(
        result.is_ok(),
        "weekly batch must not panic on zero trades: {:?}",
        result.err()
    );

    let params = fetch_strategy_params(&pool).await.unwrap();
    assert_eq!(params["atr_baseline_bars"].as_i64(), Some(50));

    let n = count_notifications_like(&pool, "%サンプル不足%")
        .await
        .unwrap();
    assert!(n >= 1, "small-sample notification should be inserted");
}
