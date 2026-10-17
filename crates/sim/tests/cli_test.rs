#![doc = "Integration tests of the command line against a database."]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use auto_trader_sim::benchmark::Benchmark;
use auto_trader_sim::cli::{self, Cli, Command, SaveRunFn};
use auto_trader_sim::config::SimConfig;
use auto_trader_sim::data;
use auto_trader_sim::error::SimError;
use auto_trader_sim::fetch;
use auto_trader_sim::script::{ParamSet, ScriptHost};
use auto_trader_sim::series::Dataset;
use auto_trader_sim::sweep::{self, RunRecord};
use auto_trader_sim::types::{Bar, M5_SECS};
use chrono::NaiveDate;
use sqlx::{PgPool, Row};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[cfg(test)]
async fn test_database_url(pool: &PgPool) -> String {
    let db_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .expect("must be able to query current_database()");
    let base_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for #[sqlx::test]");
    let (base, query) = base_url
        .split_once('?')
        .map(|(b, q)| (b, Some(q)))
        .unwrap_or((&base_url, None));
    let replaced = match base.rfind('/') {
        Some(last_slash) => format!("{}/{db_name}", &base[..last_slash]),
        None => format!("{base}/{db_name}"),
    };
    match query {
        Some(q) => format!("{replaced}?{q}"),
        None => replaced,
    }
}

#[cfg(test)]
fn write_config(name: &str, database_url: &str, sim_extra_lines: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "auto-trader-sim-cli-test-{}-{name}.toml",
        std::process::id()
    ));
    let contents = format!("[database]\nurl = \"{database_url}\"\n\n[sim]\n{sim_extra_lines}\n");
    std::fs::write(&path, contents).expect("write temp config");
    path
}

#[cfg(test)]
fn write_script(name: &str, contents: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "auto-trader-sim-cli-test-{}-{name}.rhai",
        std::process::id()
    ));
    std::fs::write(&path, contents).expect("write temp script");
    path
}

#[cfg(test)]
fn utc_secs(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(hh, mm, 0)
        .unwrap()
        .and_utc()
        .timestamp()
}

fn deterministic_bars(n: usize, start_secs: i64) -> Vec<Bar> {
    const SPREAD_MILLI: i64 = 10;
    let mid_closes: Vec<f64> = (0..n)
        .map(|i| 150.0 + 0.3 * (i as f64 * 0.07).sin() + 0.001 * i as f64)
        .collect();
    let close_milli: Vec<i64> = mid_closes
        .iter()
        .map(|&v| (v * 1000.0).round() as i64)
        .collect();
    (0..n)
        .map(|i| {
            let close = close_milli[i];
            let open = if i == 0 {
                close_milli[0]
            } else {
                close_milli[i - 1]
            };
            let half = SPREAD_MILLI / 2;
            let hi = open.max(close);
            let lo = open.min(close);
            Bar {
                open_time: start_secs + i as i64 * M5_SECS,
                bid_open: open - half,
                bid_high: hi - half,
                bid_low: lo - half,
                bid_close: close - half,
                ask_open: open + half,
                ask_high: hi + half,
                ask_low: lo + half,
                ask_close: close + half,
            }
        })
        .collect()
}

#[cfg(test)]
async fn run_capturing_json(cli: Cli, config_path: &Path) -> (i32, serde_json::Value) {
    let mut out: Vec<u8> = Vec::new();
    let code = cli::run_with_config_path_and_output(cli, config_path, &mut out).await;
    let text = String::from_utf8(out).expect("output must be UTF-8");
    let value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("output must be a single JSON document ({e}): {text:?}"));
    (code, value)
}

const EVAL_BAR_COUNT: i32 = 288;

#[cfg(test)]
async fn seed_deterministic_bars(pool: &PgPool) {
    let bars = deterministic_bars(600, utc_secs(2024, 1, 1, 0, 0));
    data::upsert_bars(pool, &bars)
        .await
        .expect("seed deterministic bars");
}

#[cfg(test)]
fn eval_from() -> NaiveDate {
    NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()
}

#[cfg(test)]
fn eval_to() -> NaiveDate {
    NaiveDate::from_ymd_opt(2024, 1, 3).unwrap()
}

fn donchian_sar_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/donchian_sar.rhai"
    ))
}

const RUNTIME_ERROR_SCRIPT: &str = "fn params() { #{} }\n\
     fn on_bar(ctx, p) {\n\
     let x = ctx.sma(\"M5\", 2000, 0);\n\
     0\n\
     }\n";

const SYNTAX_ERROR_SCRIPT: &str = "fn params( { this is not valid rhai {{{\n";

fn too_many_combinations_script() -> String {
    let mut params = String::new();
    for i in 0..12 {
        params.push_str(&format!(
            "        p{i}: #{{ min: 0, max: 1000, step: 1, \"default\": 0 }},\n"
        ));
    }
    format!("fn params() {{\n    #{{\n{params}    }}\n}}\n\nfn on_bar(ctx, p) {{ 0 }}\n")
}

#[cfg(test)]
async fn batch_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sim_batches")
        .fetch_one(pool)
        .await
        .expect("count sim_batches")
}

#[cfg(test)]
async fn script_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sim_scripts")
        .fetch_one(pool)
        .await
        .expect("count sim_scripts")
}

#[cfg(test)]
async fn run_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sim_runs")
        .fetch_one(pool)
        .await
        .expect("count sim_runs")
}

#[cfg(test)]
async fn batch_status_and_finished_at(
    pool: &PgPool,
) -> (String, Option<chrono::DateTime<chrono::Utc>>) {
    let row = sqlx::query("SELECT status, finished_at FROM sim_batches")
        .fetch_one(pool)
        .await
        .expect("exactly one sim_batches row");
    (row.get("status"), row.get("finished_at"))
}

fn panicking_run_one(
    _dataset: &Arc<Dataset>,
    _host: &ScriptHost,
    _compiled: &auto_trader_sim::script::CompiledScript,
    _params: &ParamSet,
    _benchmarks: &[Benchmark],
    _cfg: &SimConfig,
) -> Result<RunRecord, SimError> {
    panic!("forced panic for test: run_one must not take down the process (spec 12 章)")
}

fn always_failing_save_run() -> SaveRunFn {
    Arc::new(|_pool, _batch_id, _record| {
        Box::pin(async {
            Err(SimError::Other(anyhow::anyhow!(
                "forced save failure for test"
            )))
        })
    })
}

fn failing_after_n_saves(n: usize) -> SaveRunFn {
    let calls = Arc::new(AtomicUsize::new(0));
    Arc::new(move |pool, batch_id, record| {
        let call_index = calls.fetch_add(1, Ordering::SeqCst);
        if call_index < n {
            Box::pin(auto_trader_sim::store::save_run(pool, batch_id, record))
        } else {
            Box::pin(async move {
                Err(SimError::Other(anyhow::anyhow!(
                    "forced save failure after {n} successful saves (test)"
                )))
            })
        }
    })
}

#[derive(Clone, Default)]
struct CapturingWriter {
    buf: Arc<std::sync::Mutex<Vec<u8>>>,
}

#[cfg(test)]
impl CapturingWriter {
    fn contents(&self) -> String {
        String::from_utf8(self.buf.lock().expect("lock poisoned").clone())
            .expect("log output must be UTF-8")
    }
}

#[cfg(test)]
impl std::io::Write for CapturingWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf
            .lock()
            .expect("lock poisoned")
            .extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingWriter {
    type Writer = CapturingWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn benchmark_succeeds_and_computes_all_configured_thetas(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("benchmark", &db_url, "warmup_bars = 100\n");

    let cli = Cli {
        command: Command::Benchmark {
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: true,
        },
    };
    let (code, output) = run_capturing_json(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();

    assert_eq!(code, 0);
    let by_theta = &output["by_theta"];
    for theta in ["20", "50", "100"] {
        let entry = by_theta
            .get(theta)
            .unwrap_or_else(|| panic!("by_theta must have an entry for theta {theta}: {output}"));
        for key in ["leg_count", "ideal_pips", "realizable_pips"] {
            assert!(
                entry.get(key).is_some(),
                "by_theta[{theta}] must have {key}: {entry}"
            );
        }
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_with_default_params_succeeds_and_persists_batch_script_and_run_rows(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("run-default", &db_url, "warmup_bars = 100\n");

    let cli = Cli {
        command: Command::Run {
            script: donchian_sar_path(),
            params: None,
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: false,
        },
    };
    let code = cli::run_with_config_path(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    assert_eq!(code, 0);

    let batch = sqlx::query("SELECT status, total_runs, bar_count, config FROM sim_batches")
        .fetch_one(&pool)
        .await
        .expect("exactly one sim_batches row");
    assert_eq!(batch.get::<String, _>("status"), "completed");
    assert_eq!(batch.get::<i32, _>("total_runs"), 1);
    assert_eq!(batch.get::<i32, _>("bar_count"), EVAL_BAR_COUNT);
    let config: serde_json::Value = batch.get("config");
    for key in [
        "warmup_bars",
        "protective_stop_pips",
        "thetas_pips",
        "max_operations_per_bar",
        "max_operations_per_run",
    ] {
        assert!(
            config.get(key).is_some(),
            "config must contain {key}: {config}"
        );
    }

    let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sim_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(run_count, 1);

    let script = sqlx::query("SELECT name, origin FROM sim_scripts")
        .fetch_one(&pool)
        .await
        .expect("exactly one sim_scripts row");
    assert_eq!(script.get::<String, _>("name"), "donchian_sar");
    assert_eq!(script.get::<String, _>("origin"), "human");
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_with_explicit_on_step_param_succeeds(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("run-explicit-param", &db_url, "warmup_bars = 100\n");

    let cli = Cli {
        command: Command::Run {
            script: donchian_sar_path(),
            params: Some(r#"{"entry": 30}"#.to_string()),
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: false,
        },
    };
    let code = cli::run_with_config_path(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    assert_eq!(code, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_with_entry_31_off_the_min_10_step_2_grid_fails_without_creating_a_batch(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("run-off-step-param", &db_url, "warmup_bars = 100\n");

    let cli = Cli {
        command: Command::Run {
            script: donchian_sar_path(),
            params: Some(r#"{"entry": 31}"#.to_string()),
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: false,
        },
    };
    let code = cli::run_with_config_path(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    assert_eq!(code, 1);
    assert_eq!(batch_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_with_unknown_param_name_fails_without_creating_a_batch(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("run-unknown-param", &db_url, "warmup_bars = 100\n");

    let cli = Cli {
        command: Command::Run {
            script: donchian_sar_path(),
            params: Some(r#"{"x": 1}"#.to_string()),
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: false,
        },
    };
    let code = cli::run_with_config_path(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    assert_eq!(code, 1);
    assert_eq!(batch_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_with_a_runtime_erroring_script_saves_script_error_row_and_completes_batch(
    pool: PgPool,
) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("run-runtime-error", &db_url, "warmup_bars = 100\n");
    let script_path = write_script("runtime-error", RUNTIME_ERROR_SCRIPT);

    let cli = Cli {
        command: Command::Run {
            script: script_path.clone(),
            params: None,
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: false,
        },
    };
    let code = cli::run_with_config_path(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    std::fs::remove_file(&script_path).ok();

    assert_eq!(
        code, 1,
        "a script_error run must exit 1, but the result is still saved"
    );

    let run_row = sqlx::query("SELECT status FROM sim_runs")
        .fetch_one(&pool)
        .await
        .expect("exactly one sim_runs row");
    assert_eq!(run_row.get::<String, _>("status"), "script_error");

    let batch_status: String = sqlx::query_scalar("SELECT status FROM sim_batches")
        .fetch_one(&pool)
        .await
        .expect("exactly one sim_batches row");
    assert_eq!(
        batch_status, "completed",
        "spec 12: a script_error run is a normal (not failed) batch outcome"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_with_a_syntax_error_script_fails_without_registering_script_or_batch(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("run-syntax-error", &db_url, "warmup_bars = 100\n");
    let script_path = write_script("syntax-error", SYNTAX_ERROR_SCRIPT);

    let cli = Cli {
        command: Command::Run {
            script: script_path.clone(),
            params: None,
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: false,
        },
    };
    let code = cli::run_with_config_path(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    std::fs::remove_file(&script_path).ok();

    assert_eq!(code, 1);
    assert_eq!(script_count(&pool).await, 0);
    assert_eq!(batch_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_fails_without_creating_a_batch_when_sim_runs_table_is_missing(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    sqlx::query("DROP TABLE sim_runs")
        .execute(&pool)
        .await
        .expect("drop must succeed as a test precondition");
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("run-missing-table", &db_url, "warmup_bars = 100\n");

    let cli = Cli {
        command: Command::Run {
            script: donchian_sar_path(),
            params: None,
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: false,
        },
    };
    let code = cli::run_with_config_path(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();

    assert_eq!(code, 1);
    let remaining_batches: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sim_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining_batches, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_marks_the_batch_failed_and_exits_1_when_run_one_panics(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("run-panics", &db_url, "warmup_bars = 100\n");

    let code = cli::run_run_with_hooks(
        &config_path,
        donchian_sar_path(),
        None,
        Some(eval_from()),
        Some(eval_to()),
        false,
        &mut std::io::sink(),
        panicking_run_one,
        cli::real_save_run(),
    )
    .await;
    std::fs::remove_file(&config_path).ok();

    assert_eq!(
        code, 1,
        "a panicking run_one must not crash the process; it must exit 1"
    );
    let (status, finished_at) = batch_status_and_finished_at(&pool).await;
    assert_eq!(status, "failed");
    assert!(
        finished_at.is_some(),
        "finished_at must be set when the batch transitions to failed"
    );
    assert_eq!(
        run_count(&pool).await,
        0,
        "a panicking run_one must not leave a sim_runs row"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_marks_the_batch_failed_and_exits_1_when_save_run_fails(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("run-save-fails", &db_url, "warmup_bars = 100\n");

    let code = cli::run_run_with_hooks(
        &config_path,
        donchian_sar_path(),
        None,
        Some(eval_from()),
        Some(eval_to()),
        false,
        &mut std::io::sink(),
        sweep::run_one,
        always_failing_save_run(),
    )
    .await;
    std::fs::remove_file(&config_path).ok();

    assert_eq!(code, 1);
    let (status, finished_at) = batch_status_and_finished_at(&pool).await;
    assert_eq!(status, "failed");
    assert!(finished_at.is_some());
    assert_eq!(
        run_count(&pool).await,
        0,
        "a failed save must not leave a sim_runs row"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweep_runs_max_runs_combinations_and_reports_top_runs_sorted_by_total_pips(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("sweep-basic", &db_url, "warmup_bars = 100\n");

    let cli = Cli {
        command: Command::Sweep {
            script: donchian_sar_path(),
            from: Some(eval_from()),
            to: Some(eval_to()),
            max_runs: 5,
            seed: 42,
            jobs: Some(2),
            json: true,
        },
    };
    let (code, output) = run_capturing_json(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    assert_eq!(code, 0);

    let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sim_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(run_count, 5);
    let ok_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sim_runs WHERE status = 'ok'")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(output["total_runs"], 5);
    let top_runs = output["top_runs"]
        .as_array()
        .expect("top_runs must be an array");
    assert!(top_runs.len() <= 5, "top_runs: {output}");
    assert_eq!(
        top_runs.len() as i64,
        ok_count,
        "with max_runs below the top-10 limit, every ok run must be listed"
    );
    let pips: Vec<f64> = top_runs
        .iter()
        .map(|r| {
            r["total_pips"]
                .as_f64()
                .expect("total_pips must be a number")
        })
        .collect();
    assert!(
        pips.windows(2).all(|w| w[0] >= w[1]),
        "top_runs must be sorted by total_pips descending: {pips:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweep_with_jobs_larger_than_run_count_still_succeeds_with_max_runs_results(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("sweep-jobs-clamped", &db_url, "warmup_bars = 100\n");

    let cli = Cli {
        command: Command::Sweep {
            script: donchian_sar_path(),
            from: Some(eval_from()),
            to: Some(eval_to()),
            max_runs: 3,
            seed: 42,
            jobs: Some(64),
            json: true,
        },
    };
    let (code, output) = run_capturing_json(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();

    assert_eq!(code, 0, "{output}");
    assert_eq!(run_count(&pool).await, 3);
    assert_eq!(output["total_runs"], 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweep_marks_the_batch_failed_and_exits_1_when_a_mid_sweep_save_fails(pool: PgPool) {
    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("sweep-save-fails", &db_url, "warmup_bars = 100\n");

    let code = cli::run_sweep_cmd_with_hooks(
        &config_path,
        donchian_sar_path(),
        Some(eval_from()),
        Some(eval_to()),
        5,
        42,
        Some(1),
        false,
        &mut std::io::sink(),
        failing_after_n_saves(2),
    )
    .await;
    std::fs::remove_file(&config_path).ok();

    assert_eq!(code, 1);
    let (status, finished_at) = batch_status_and_finished_at(&pool).await;
    assert_eq!(status, "failed");
    assert!(finished_at.is_some());
    let saved = run_count(&pool).await;
    assert!(
        saved < 5,
        "a mid-sweep save failure must leave fewer sim_runs rows than total_runs, got {saved}"
    );
    assert_eq!(
        saved, 2,
        "exactly the 2 successful saves before the forced failure must persist"
    );
}

#[tokio::test]
async fn sweep_without_max_runs_fails_via_clap() {
    let code = cli::run_args([
        "auto-trader-sim",
        "sweep",
        "--script",
        "unused.rhai",
        "--jobs",
        "1",
    ])
    .await;
    assert_eq!(
        code, 1,
        "missing required --max-runs must be a clap error (exit 1)"
    );
}

#[tokio::test]
async fn sweep_with_jobs_zero_fails_via_clap() {
    let code = cli::run_args([
        "auto-trader-sim",
        "sweep",
        "--script",
        "unused.rhai",
        "--max-runs",
        "5",
        "--jobs",
        "0",
    ])
    .await;
    assert_eq!(code, 1, "--jobs 0 must be rejected, not treated as 1");
}

#[tokio::test]
async fn sweep_with_max_runs_zero_fails_via_clap() {
    let code = cli::run_args([
        "auto-trader-sim",
        "sweep",
        "--script",
        "unused.rhai",
        "--max-runs",
        "0",
    ])
    .await;
    assert_eq!(code, 1);
}

#[tokio::test]
async fn sweep_with_max_runs_above_one_million_fails_via_clap() {
    let code = cli::run_args([
        "auto-trader-sim",
        "sweep",
        "--script",
        "unused.rhai",
        "--max-runs",
        "1000001",
    ])
    .await;
    assert_eq!(code, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweep_with_more_than_two_pow_63_combinations_fails_without_creating_a_batch(pool: PgPool) {
    let precondition_script = too_many_combinations_script();
    let host = ScriptHost::new(&SimConfig::default());
    let compiled = host
        .compile(&precondition_script)
        .expect("precondition: the script must compile so only the combination limit rejects it");
    assert!(
        matches!(
            sweep::total_combinations(&compiled.params),
            Err(SimError::Args(_))
        ),
        "precondition: total_combinations must reject this script with SimError::Args"
    );

    seed_deterministic_bars(&pool).await;
    let db_url = test_database_url(&pool).await;
    let config_path = write_config(
        "sweep-too-many-combinations",
        &db_url,
        "warmup_bars = 100\n",
    );
    let script_path = write_script("too-many-combinations", &too_many_combinations_script());

    let cli = Cli {
        command: Command::Sweep {
            script: script_path.clone(),
            from: Some(eval_from()),
            to: Some(eval_to()),
            max_runs: 10,
            seed: 42,
            jobs: Some(1),
            json: false,
        },
    };
    let code = cli::run_with_config_path(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    std::fs::remove_file(&script_path).ok();

    assert_eq!(code, 1);
    assert_eq!(batch_count(&pool).await, 0);
}

fn success_kline_body() -> serde_json::Value {
    serde_json::json!({
        "status": 0,
        "data": [{
            "openTime": "1698451200000",
            "open": "149.605",
            "high": "149.612",
            "low": "149.601",
            "close": "149.610"
        }],
        "responsetime": "2023-10-28T00:05:00.000Z"
    })
}

#[sqlx::test(migrations = "../../migrations")]
async fn backfill_succeeds_when_all_requests_succeed(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .and(query_param("date", "20231028"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_kline_body()))
        .mount(&server)
        .await;

    let db_url = test_database_url(&pool).await;
    let config_path = write_config(
        "backfill-success",
        &db_url,
        &format!("gmo_public_base_url = \"{}\"\n", server.uri()),
    );
    let day = NaiveDate::from_ymd_opt(2023, 10, 28).unwrap();
    let cli = Cli {
        command: Command::Backfill {
            from: day,
            to: day,
            json: true,
        },
    };
    let (code, output) = run_capturing_json(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    assert_eq!(code, 0);
    assert_eq!(output["days"], 1);
    assert_eq!(output["failed"], serde_json::json!([] as [String; 0]));
}

#[sqlx::test(migrations = "../../migrations")]
async fn backfill_reports_counts_and_failures_when_one_of_two_days_fails_after_waiting_out_the_real_retry_delays(
    pool: PgPool,
) {
    let server = MockServer::start().await;
    for (date, ask_status) in [("20231028", 200u16), ("20231029", 500u16)] {
        Mock::given(method("GET"))
            .and(path("/v1/klines"))
            .and(query_param("priceType", "BID"))
            .and(query_param("date", date))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_kline_body()))
            .mount(&server)
            .await;
        let ask_response = if ask_status == 200 {
            ResponseTemplate::new(200).set_body_json(success_kline_body())
        } else {
            ResponseTemplate::new(ask_status)
        };
        Mock::given(method("GET"))
            .and(path("/v1/klines"))
            .and(query_param("priceType", "ASK"))
            .and(query_param("date", date))
            .respond_with(ask_response)
            .mount(&server)
            .await;
    }

    let db_url = test_database_url(&pool).await;
    let config_path = write_config(
        "backfill-partial",
        &db_url,
        &format!("gmo_public_base_url = \"{}\"\n", server.uri()),
    );
    let cli = Cli {
        command: Command::Backfill {
            from: NaiveDate::from_ymd_opt(2023, 10, 28).unwrap(),
            to: NaiveDate::from_ymd_opt(2023, 10, 29).unwrap(),
            json: true,
        },
    };
    let (code, output) = run_capturing_json(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();

    assert_eq!(code, 1, "a partial failure must exit 1: {output}");
    assert_eq!(output["days"], 2, "{output}");
    assert!(
        output["saved"].as_u64().expect("saved must be a number") >= 1,
        "the successful day's bars must be counted even though another day failed: {output}"
    );
    assert_eq!(output["failed"], serde_json::json!(["20231029 ASK"]));
}

#[tokio::test]
async fn backfill_with_from_after_to_fails_with_an_argument_error_before_connecting_to_the_database()
 {
    let config_path = write_config(
        "backfill-from-after-to",
        "postgres://127.0.0.1:1/unreachable",
        "",
    );
    let cli = Cli {
        command: Command::Backfill {
            from: NaiveDate::from_ymd_opt(2023, 10, 29).unwrap(),
            to: NaiveDate::from_ymd_opt(2023, 10, 28).unwrap(),
            json: false,
        },
    };

    let writer = CapturingWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer.clone())
        .with_ansi(false)
        .finish();
    let code = {
        let _guard = tracing::subscriber::set_default(subscriber);
        cli::run_with_config_path(cli, &config_path).await
    };
    std::fs::remove_file(&config_path).ok();

    assert_eq!(code, 1);
    let log = writer.contents();
    assert!(
        log.contains("invalid --from: must not be after --to"),
        "expected the argument-validation error message in the logs, got: {log}"
    );
    assert!(
        !log.contains("failed to connect to database"),
        "argument validation must happen before any DB connection attempt, got: {log}"
    );
}

#[tokio::test]
async fn backfill_with_from_before_the_gmo_kline_min_date_fails_with_an_argument_error_before_connecting_to_the_database()
 {
    let min_date = fetch::gmo_kline_min_date();
    let config_path = write_config(
        "backfill-before-min-date",
        "postgres://127.0.0.1:1/unreachable",
        "",
    );
    let cli = Cli {
        command: Command::Backfill {
            from: min_date
                .pred_opt()
                .expect("gmo_kline_min_date is not NaiveDate::MIN"),
            to: min_date,
            json: false,
        },
    };

    let writer = CapturingWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer.clone())
        .with_ansi(false)
        .finish();
    let code = {
        let _guard = tracing::subscriber::set_default(subscriber);
        cli::run_with_config_path(cli, &config_path).await
    };
    std::fs::remove_file(&config_path).ok();

    assert_eq!(code, 1);
    let log = writer.contents();
    assert!(
        log.contains(&min_date.to_string()),
        "expected the argument-validation error message to mention the oldest accepted date \
         ({min_date}), got: {log}"
    );
    assert!(
        !log.contains("failed to connect to database"),
        "argument validation must happen before any DB connection attempt, got: {log}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn backfill_fails_when_one_day_fails_to_fetch_after_waiting_out_the_real_retry_delays(
    pool: PgPool,
) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .and(query_param("priceType", "BID"))
        .and(query_param("date", "20231028"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_kline_body()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .and(query_param("priceType", "ASK"))
        .and(query_param("date", "20231028"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let db_url = test_database_url(&pool).await;
    let config_path = write_config(
        "backfill-failure",
        &db_url,
        &format!("gmo_public_base_url = \"{}\"\n", server.uri()),
    );
    let day = NaiveDate::from_ymd_opt(2023, 10, 28).unwrap();
    let cli = Cli {
        command: Command::Backfill {
            from: day,
            to: day,
            json: false,
        },
    };
    let code = cli::run_with_config_path(cli, &config_path).await;
    std::fs::remove_file(&config_path).ok();
    assert_eq!(code, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalid_sim_config_fails_every_subcommand(pool: PgPool) {
    let db_url = test_database_url(&pool).await;
    let config_path = write_config("invalid-jobs", &db_url, "jobs = 0\n");
    let script_path = donchian_sar_path();

    let commands = vec![
        Command::Migrate,
        Command::Backfill {
            from: eval_from(),
            to: eval_to(),
            json: false,
        },
        Command::Benchmark {
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: false,
        },
        Command::Run {
            script: script_path.clone(),
            params: None,
            from: Some(eval_from()),
            to: Some(eval_to()),
            json: false,
        },
        Command::Sweep {
            script: script_path,
            from: Some(eval_from()),
            to: Some(eval_to()),
            max_runs: 5,
            seed: 42,
            jobs: Some(1),
            json: false,
        },
    ];

    for command in commands {
        let label = format!("{command:?}");
        let code = cli::run_with_config_path(Cli { command }, &config_path).await;
        assert_eq!(
            code, 1,
            "invalid sim.jobs=0 must fail every subcommand: {label}"
        );
    }

    std::fs::remove_file(&config_path).ok();
}
