use auto_trader_sim::config::SimConfig;
use auto_trader_sim::engine::{ExitReason, RunStatus, SimOutcome, SimTrade};
use auto_trader_sim::error::SimError;
use auto_trader_sim::eval::{self, Metrics};
use auto_trader_sim::script::{ParamSet, ParamValue, ScriptHost};
use auto_trader_sim::store::{self, BatchStatus, NewBatch};
use auto_trader_sim::sweep::RunRecord;
use auto_trader_sim::types::Bar;
use sqlx::PgPool;
use uuid::Uuid;

fn compile_minimal_script(source: &str) -> auto_trader_sim::script::CompiledScript {
    let host = ScriptHost::new(&SimConfig::default());
    host.compile(source).expect("test script must be valid")
}

const MINIMAL_SCRIPT: &str = "fn params() { #{} }\nfn on_bar(ctx, p) { 0 }\n";

fn sample_metrics(total_pips: f64) -> Metrics {
    let bar = Bar {
        open_time: 0,
        bid_open: 150_000,
        bid_high: 150_000,
        bid_low: 150_000,
        bid_close: 150_000,
        ask_open: 150_010,
        ask_high: 150_010,
        ask_low: 150_010,
        ask_close: 150_010,
    };
    let pnl_milli = (total_pips * 10.0).round() as i64;
    let trade = SimTrade {
        direction: 1,
        entry_idx: 0,
        exit_idx: 0,
        entry_milli: 0,
        exit_milli: pnl_milli,
        pnl_milli,
        reason: ExitReason::Signal,
    };
    let outcome = SimOutcome {
        status: RunStatus::Ok,
        trades: vec![trade],
        positions: vec![1],
    };
    eval::evaluate(&[bar], &outcome, &[])
}

fn params_with_entry(entry: i64) -> ParamSet {
    let mut params = ParamSet::new();
    params.insert("entry".to_string(), ParamValue::Int(entry));
    params
}

fn ok_record(entry: i64, total_pips: f64) -> RunRecord {
    RunRecord {
        params: params_with_entry(entry),
        status: RunStatus::Ok,
        metrics: Some(sample_metrics(total_pips)),
    }
}

async fn setup_script_and_batch(pool: &PgPool) -> (Uuid, Uuid) {
    let script = compile_minimal_script(MINIMAL_SCRIPT);
    let script_id = store::register_script(pool, "minimal", &script, "human", None)
        .await
        .expect("register_script must succeed");
    let batch_id = store::create_batch(
        pool,
        &NewBatch {
            script_id,
            period_from: 0,
            period_to: 300,
            bar_count: 1,
            total_runs: 1,
            config: serde_json::json!({}),
        },
    )
    .await
    .expect("create_batch must succeed");
    (script_id, batch_id)
}

#[sqlx::test(migrations = "../../migrations")]
async fn register_script_is_idempotent_by_source_sha256_and_keeps_the_first_name(pool: PgPool) {
    let script = compile_minimal_script(MINIMAL_SCRIPT);

    let id1 = store::register_script(&pool, "first_name", &script, "human", None)
        .await
        .expect("first registration must succeed");
    let id2 = store::register_script(&pool, "second_name", &script, "human", None)
        .await
        .expect("second registration of the same source must succeed");

    assert_eq!(id1, id2, "same source_sha256 must yield the same id");

    let row_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sim_scripts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count, 1, "sim_scripts must have exactly one row");

    let stored_name: String = sqlx::query_scalar("SELECT name FROM sim_scripts WHERE id = $1")
        .bind(id1)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        stored_name, "first_name",
        "the second registration must not overwrite the name"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_batch_starts_running_with_no_finished_at(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;

    let (status, finished_at): (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, finished_at FROM sim_batches WHERE id = $1")
            .bind(batch_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, "running");
    assert!(finished_at.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_batch_rejects_a_period_that_is_not_a_representable_instant(pool: PgPool) {
    let script = compile_minimal_script(MINIMAL_SCRIPT);
    let script_id = store::register_script(&pool, "minimal", &script, "human", None)
        .await
        .expect("register_script must succeed");

    let err = store::create_batch(
        &pool,
        &NewBatch {
            script_id,
            period_from: i64::MAX,
            period_to: i64::MAX,
            bar_count: 1,
            total_runs: 1,
            config: serde_json::json!({}),
        },
    )
    .await
    .expect_err("an unrepresentable period_from must be rejected, not panic");

    assert!(matches!(err, SimError::Other(_)), "got {err:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn finish_batch_completed_sets_status_and_finished_at(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;

    store::finish_batch(&pool, batch_id, BatchStatus::Completed)
        .await
        .expect("finish_batch(Completed) must succeed");

    let (status, finished_at): (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, finished_at FROM sim_batches WHERE id = $1")
            .bind(batch_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, "completed");
    assert!(finished_at.is_some());
}

#[sqlx::test(migrations = "../../migrations")]
async fn finish_batch_failed_sets_status_and_finished_at(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;

    store::finish_batch(&pool, batch_id, BatchStatus::Failed)
        .await
        .expect("finish_batch(Failed) must succeed");

    let (status, finished_at): (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, finished_at FROM sim_batches WHERE id = $1")
            .bind(batch_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, "failed");
    assert!(finished_at.is_some());
}

#[sqlx::test(migrations = "../../migrations")]
async fn finish_batch_for_an_unknown_batch_id_is_an_error(pool: PgPool) {
    let err = store::finish_batch(&pool, Uuid::new_v4(), BatchStatus::Completed)
        .await
        .expect_err("a non-existent batch must not be reported as finished");

    assert!(matches!(err, SimError::Args(_)), "got {err:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn finish_batch_on_an_already_failed_batch_is_an_error_and_keeps_failed(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    store::finish_batch(&pool, batch_id, BatchStatus::Failed)
        .await
        .expect("running -> failed must succeed");

    let err = store::finish_batch(&pool, batch_id, BatchStatus::Completed)
        .await
        .expect_err("failed -> completed must be rejected");
    assert!(matches!(err, SimError::Args(_)), "got {err:?}");

    let status: String = sqlx::query_scalar("SELECT status FROM sim_batches WHERE id = $1")
        .bind(batch_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "failed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn finish_batch_with_running_is_an_error(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;

    let err = store::finish_batch(&pool, batch_id, BatchStatus::Running)
        .await
        .expect_err("transition to running must be rejected");
    assert!(matches!(err, SimError::Args(_)), "got {err:?}");
}

#[derive(sqlx::FromRow)]
struct SimRunRow {
    status: String,
    error: Option<String>,
    total_pips: Option<f64>,
    trade_count: Option<i32>,
    win_rate: Option<f64>,
    max_drawdown_pips: Option<f64>,
    time_in_market: Option<f64>,
    protective_stop_count: Option<i32>,
    metrics: Option<serde_json::Value>,
}

async fn fetch_run(pool: &PgPool, run_id: Uuid) -> SimRunRow {
    sqlx::query_as(
        "SELECT status, error, total_pips, trade_count, win_rate, max_drawdown_pips, \
         time_in_market, protective_stop_count, metrics FROM sim_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn save_run_ok_stores_metric_columns_and_metrics_json_with_null_error(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    let record = ok_record(20, 42.5);

    let run_id = store::save_run(&pool, batch_id, &record)
        .await
        .expect("save_run(Ok) must succeed");
    let row = fetch_run(&pool, run_id).await;

    assert_eq!(row.status, "ok");
    assert!(row.error.is_none());
    assert_eq!(row.total_pips, Some(42.5));
    assert_eq!(row.trade_count, Some(1));
    let expected = record.metrics.as_ref().unwrap();
    assert_eq!(row.win_rate, Some(expected.win_rate));
    assert_eq!(row.max_drawdown_pips, Some(expected.max_drawdown_pips));
    assert_eq!(row.time_in_market, Some(expected.time_in_market));
    assert_eq!(
        row.protective_stop_count,
        Some(expected.protective_stop_count as i32)
    );
    assert_eq!(row.metrics, Some(expected.metrics_json()));
}

#[sqlx::test(migrations = "../../migrations")]
async fn save_run_script_error_formats_error_with_rfc3339_prefix_and_nulls_metric_columns(
    pool: PgPool,
) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    let open_time = 1_700_000_000i64;
    let record = RunRecord {
        params: params_with_entry(20),
        status: RunStatus::ScriptError {
            open_time,
            message: "division by zero".to_string(),
        },
        metrics: None,
    };

    let run_id = store::save_run(&pool, batch_id, &record)
        .await
        .expect("save_run(ScriptError) must succeed");
    let row = fetch_run(&pool, run_id).await;

    assert_eq!(row.status, "script_error");
    let expected_prefix = chrono::DateTime::<chrono::Utc>::from_timestamp(open_time, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    assert_eq!(
        row.error,
        Some(format!("{expected_prefix} division by zero"))
    );
    assert!(row.total_pips.is_none());
    assert!(row.trade_count.is_none());
    assert!(row.win_rate.is_none());
    assert!(row.max_drawdown_pips.is_none());
    assert!(row.time_in_market.is_none());
    assert!(row.protective_stop_count.is_none());
    assert!(row.metrics.is_none());
}

async fn run_row_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sim_runs")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn save_run_rejects_ok_status_without_metrics_and_writes_no_row(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    let record = RunRecord {
        params: params_with_entry(20),
        status: RunStatus::Ok,
        metrics: None,
    };

    let err = store::save_run(&pool, batch_id, &record)
        .await
        .expect_err("Ok without metrics must be rejected");

    assert!(matches!(err, SimError::Other(_)), "got {err:?}");
    assert_eq!(run_row_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn save_run_rejects_script_error_status_with_metrics_and_writes_no_row(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    let record = RunRecord {
        params: params_with_entry(20),
        status: RunStatus::ScriptError {
            open_time: 0,
            message: "boom".to_string(),
        },
        metrics: Some(sample_metrics(1.0)),
    };

    let err = store::save_run(&pool, batch_id, &record)
        .await
        .expect_err("ScriptError with metrics must be rejected");

    assert!(matches!(err, SimError::Other(_)), "got {err:?}");
    assert_eq!(run_row_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn save_run_rejects_script_error_open_time_that_is_not_a_representable_instant(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    let record = RunRecord {
        params: params_with_entry(20),
        status: RunStatus::ScriptError {
            open_time: i64::MAX,
            message: "boom".to_string(),
        },
        metrics: None,
    };

    let err = store::save_run(&pool, batch_id, &record)
        .await
        .expect_err("unrepresentable open_time must be rejected, not panic");

    assert!(matches!(err, SimError::Other(_)), "got {err:?}");
    assert_eq!(run_row_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn save_run_rejects_trade_count_that_does_not_fit_i32_and_writes_no_row(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    let mut metrics = sample_metrics(1.0);
    metrics.trade_count = i32::MAX as usize + 1;
    let record = RunRecord {
        params: params_with_entry(20),
        status: RunStatus::Ok,
        metrics: Some(metrics),
    };

    let err = store::save_run(&pool, batch_id, &record)
        .await
        .expect_err("out-of-range trade_count must be rejected");

    assert!(matches!(err, SimError::Other(_)), "got {err:?}");
    assert_eq!(run_row_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn save_run_rejects_protective_stop_count_that_does_not_fit_i32(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    let mut metrics = sample_metrics(1.0);
    metrics.protective_stop_count = i32::MAX as usize + 1;
    let record = RunRecord {
        params: params_with_entry(20),
        status: RunStatus::Ok,
        metrics: Some(metrics),
    };

    let err = store::save_run(&pool, batch_id, &record)
        .await
        .expect_err("out-of-range protective_stop_count must be rejected");

    assert!(matches!(err, SimError::Other(_)), "got {err:?}");
    assert_eq!(run_row_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn register_script_rejects_a_human_origin_with_a_parent_and_writes_no_row(pool: PgPool) {
    let (parent_id, _) = setup_script_and_batch(&pool).await;
    let child = compile_minimal_script("fn params() { #{} }\nfn on_bar(ctx, p) { 1 }\n");

    let err = store::register_script(&pool, "child", &child, "human", Some(parent_id))
        .await
        .expect_err("human origin must not have a parent_id");

    assert!(matches!(err, SimError::Args(_)), "got {err:?}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sim_scripts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "only the parent row must exist");
}

#[sqlx::test(migrations = "../../migrations")]
async fn save_run_rejects_a_second_row_with_the_same_batch_id_and_params(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    let record = ok_record(20, 10.0);

    store::save_run(&pool, batch_id, &record)
        .await
        .expect("first save_run must succeed");
    let err = store::save_run(&pool, batch_id, &record)
        .await
        .expect_err("duplicate (batch_id, params) must be rejected");

    assert!(
        matches!(err, SimError::Db(_)),
        "expected SimError::Db from the UNIQUE (batch_id, params) violation, got {err:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn top_runs_is_empty_for_a_running_batch(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    store::save_run(&pool, batch_id, &ok_record(20, 10.0))
        .await
        .unwrap();

    let top = store::top_runs(&pool, batch_id, 10).await.unwrap();
    assert!(
        top.is_empty(),
        "a running batch must not appear in top_runs"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn top_runs_is_empty_for_a_failed_batch(pool: PgPool) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    store::save_run(&pool, batch_id, &ok_record(20, 10.0))
        .await
        .unwrap();
    store::finish_batch(&pool, batch_id, BatchStatus::Failed)
        .await
        .unwrap();

    let top = store::top_runs(&pool, batch_id, 10).await.unwrap();
    assert!(top.is_empty(), "a failed batch must not appear in top_runs");
}

#[sqlx::test(migrations = "../../migrations")]
async fn top_runs_orders_by_total_pips_desc_then_params_text_asc_for_a_completed_batch(
    pool: PgPool,
) {
    let (_, batch_id) = setup_script_and_batch(&pool).await;
    store::save_run(&pool, batch_id, &ok_record(10, 30.0))
        .await
        .unwrap();
    store::save_run(&pool, batch_id, &ok_record(20, 30.0))
        .await
        .unwrap();
    store::save_run(&pool, batch_id, &ok_record(30, 10.0))
        .await
        .unwrap();
    store::finish_batch(&pool, batch_id, BatchStatus::Completed)
        .await
        .unwrap();

    let top = store::top_runs(&pool, batch_id, 10).await.unwrap();
    assert_eq!(top.len(), 3);
    assert_eq!(top[0].total_pips, 30.0);
    assert_eq!(top[1].total_pips, 30.0);
    assert_eq!(top[2].total_pips, 10.0);
    assert_eq!(top[0].params["entry"], 10);
    assert_eq!(top[1].params["entry"], 20);
    assert_eq!(top[2].params["entry"], 30);
}
