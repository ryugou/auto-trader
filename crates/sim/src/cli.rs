use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use crate::benchmark::{self, Benchmark};
use crate::config::{self, SimConfig};
use crate::data;
use crate::engine::RunStatus;
use crate::error::SimError;
use crate::fetch::{self, GmoKlineClient};
use crate::script::{CompiledScript, ParamSet, ScriptHost};
use crate::series::{self, Dataset};
use crate::store::{self, BatchStatus, NewBatch};
use crate::sweep::{self, RunRecord, run_sweep};
use crate::types::Bar;

mod args;
mod dispatch;
mod migrate;
mod output;

pub use args::{Cli, Command};
pub use dispatch::{run, run_args, run_with_config_path, run_with_config_path_and_output};

use output::{output_failed, print_backfill, print_benchmark, print_run, print_sweep};

const SWEEP_TOP_RUNS_LIMIT: i64 = 10;

pub fn resolve_period(
    all_bars: &[Bar],
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    warmup_bars: usize,
) -> Result<(i64, i64), SimError> {
    if all_bars.is_empty() {
        return Err(SimError::Args(
            "invalid sim_candles: no bars are loaded; run `backfill` first".to_string(),
        ));
    }

    let from_secs = match from {
        Some(d) => naive_date_to_utc_secs(d),
        None => series::earliest_from(all_bars, warmup_bars).ok_or_else(|| {
            SimError::Args(format!(
                "invalid default --from: fewer than warmup_bars={warmup_bars} \
                 bars are loaded before any candidate start (have {} bars total); pass --from \
                 explicitly or run `backfill` to load more history",
                all_bars.len()
            ))
        })?,
    };
    let to_secs = match to {
        Some(d) => naive_date_to_utc_secs(d),
        None => {
            let last_bar = all_bars
                .last()
                .expect("checked all_bars is non-empty above");
            let last_date = chrono::DateTime::<chrono::Utc>::from_timestamp(last_bar.open_time, 0)
                .expect("Bar.open_time must be a valid UTC timestamp (data.rs invariant)")
                .date_naive();
            let next_date = last_date.succ_opt().expect(
                "date arithmetic must not overflow for real market data (far from NaiveDate::MAX)",
            );
            naive_date_to_utc_secs(next_date)
        }
    };

    if from_secs >= to_secs {
        return Err(SimError::Args(format!(
            "invalid evaluation period: [from, to) is empty or inverted (from={from_secs} to={to_secs} \
             UTC epoch seconds); --from must be strictly before --to"
        )));
    }

    Ok((from_secs, to_secs))
}

fn naive_date_to_utc_secs(date: NaiveDate) -> i64 {
    date.and_time(chrono::NaiveTime::MIN).and_utc().timestamp()
}

struct Prepared {
    pool: PgPool,
    sim: SimConfig,
}

async fn prepare(config_path: &Path) -> Result<Prepared, i32> {
    let settings = config::load(config_path).map_err(|e| {
        tracing::error!(
            config_path = %config_path.display(),
            error = %e,
            "failed to load or validate sim config"
        );
        1
    })?;
    let pool = data::connect(&settings.database_url).await.map_err(|e| {
        tracing::error!(error = %e, "failed to connect to database");
        1
    })?;
    data::ensure_tables(&pool).await.map_err(|e| {
        tracing::error!(error = %e, "invalid database: required sim_* tables are missing");
        1
    })?;
    Ok(Prepared {
        pool,
        sim: settings.sim,
    })
}

async fn fail_batch_and_exit(pool: &PgPool, batch_id: Uuid) -> i32 {
    if let Err(e) = store::finish_batch(pool, batch_id, BatchStatus::Failed).await {
        tracing::error!(
            batch_id = %batch_id,
            error = %e,
            "failed to mark sim_batches row as failed: \
             the row is stuck in 'running' state and needs manual investigation"
        );
    }
    1
}

#[doc(hidden)]
pub type RunOneFn = fn(
    &std::sync::Arc<Dataset>,
    &ScriptHost,
    &CompiledScript,
    &ParamSet,
    &[Benchmark],
    &SimConfig,
) -> RunRecord;

#[doc(hidden)]
pub type SaveRunFn = std::sync::Arc<
    dyn for<'a> Fn(
            &'a PgPool,
            Uuid,
            &'a RunRecord,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Uuid, SimError>> + Send + 'a>,
        > + Send
        + Sync,
>;

#[doc(hidden)]
pub fn real_save_run() -> SaveRunFn {
    std::sync::Arc::new(|pool, batch_id, record| Box::pin(store::save_run(pool, batch_id, record)))
}

fn warn_missing_weekdays(dataset: &Dataset) {
    for date in dataset.missing_weekdays() {
        tracing::warn!(
            date = %date,
            "invalid evaluation period: no bars for this utc weekday"
        );
    }
}

fn read_and_compile(host: &ScriptHost, script_path: &Path) -> Result<CompiledScript, i32> {
    let source = std::fs::read_to_string(script_path).map_err(|e| {
        tracing::error!(script = %script_path.display(), error = %e, "failed to read script file");
        1
    })?;
    host.compile(&source).map_err(|e| {
        tracing::error!(script = %script_path.display(), error = %e, "invalid script: failed registration validation");
        1
    })
}

async fn build_dataset(
    prepared: &Prepared,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<(Dataset, i64, i64), i32> {
    let all_bars = data::load_bars(&prepared.pool, None).await.map_err(|e| {
        tracing::error!(error = %e, "failed to load bars from sim_candles");
        1
    })?;
    let (from_secs, to_secs) = resolve_period(&all_bars, from, to, prepared.sim.warmup_bars)
        .map_err(|e| {
            tracing::error!(error = %e, "failed to resolve evaluation period");
            1
        })?;
    let dataset = Dataset::new(
        all_bars,
        from_secs,
        to_secs,
        prepared.sim.warmup_bars,
        prepared.sim.indicator_cache_mb,
    )
    .map_err(|e| {
        tracing::error!(error = %e, "failed to build dataset for the evaluation period");
        1
    })?;
    warn_missing_weekdays(&dataset);
    Ok((dataset, from_secs, to_secs))
}

fn compute_benchmarks(dataset: &Dataset, sim: &SimConfig) -> Vec<Benchmark> {
    sim.thetas_pips
        .iter()
        .map(|&theta| benchmark::compute(dataset.eval_bars(), theta))
        .collect()
}

struct EvalInputs {
    dataset: Dataset,
    from_secs: i64,
    to_secs: i64,
    bar_count: i32,
    benchmarks: Vec<Benchmark>,
}

async fn load_eval_inputs(
    prepared: &Prepared,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<EvalInputs, i32> {
    let (dataset, from_secs, to_secs) = build_dataset(prepared, from, to).await?;
    let bar_count = i32::try_from(dataset.eval_bars().len()).map_err(|e| {
        tracing::error!(
            error = %e,
            eval_bars = dataset.eval_bars().len(),
            "invalid evaluation period bar count: does not fit sim_batches.bar_count as i32"
        );
        1
    })?;
    let benchmarks = compute_benchmarks(&dataset, &prepared.sim);
    Ok(EvalInputs {
        dataset,
        from_secs,
        to_secs,
        bar_count,
        benchmarks,
    })
}

fn script_name_from_path(path: &Path) -> Result<String, SimError> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            SimError::Args(format!(
                "invalid script path: cannot derive a script name from {} (expected a file name with a valid \
                 UTF-8 stem)",
                path.display()
            ))
        })
}

fn parse_given_params(
    raw: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, SimError> {
    let Some(raw) = raw else {
        return Ok(serde_json::Map::new());
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(other) => Err(SimError::Args(format!(
            "invalid --params: must be a JSON object (got {other})"
        ))),
        Err(e) => Err(SimError::Args(format!(
            "invalid --params: not valid JSON: {e} (input={raw:?})"
        ))),
    }
}

fn run_batch_config_json(sim: &SimConfig) -> serde_json::Value {
    serde_json::json!({
        "warmup_bars": sim.warmup_bars,
        "protective_stop_pips": sim.protective_stop_pips,
        "thetas_pips": sim.thetas_pips,
        "max_operations_per_bar": sim.max_operations_per_bar,
        "max_operations_per_run": sim.max_operations_per_run,
    })
}

fn sweep_batch_config_json(sim: &SimConfig, max_runs: usize, seed: u64) -> serde_json::Value {
    let mut value = run_batch_config_json(sim);
    value["max_runs"] = serde_json::json!(max_runs);
    value["seed"] = serde_json::json!(seed);
    value
}

fn effective_jobs(requested: usize, run_count: usize, available: usize) -> usize {
    requested.min(run_count).min(available).max(1)
}

async fn run_backfill(
    config_path: &Path,
    from: NaiveDate,
    to: NaiveDate,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    if from > to {
        tracing::error!(from = %from, to = %to, "invalid --from: must not be after --to");
        return 1;
    }
    let min_date = fetch::gmo_kline_min_date();
    if from < min_date {
        tracing::error!(
            from = %from,
            min_date = %min_date,
            "invalid --from: earlier than the oldest date the gmo kline api accepts; pass --from min_date or later"
        );
        return 1;
    }
    let prepared = match prepare(config_path).await {
        Ok(p) => p,
        Err(code) => return code,
    };

    let client = GmoKlineClient::new(&prepared.sim.gmo_public_base_url);

    let mut total = fetch::BackfillReport {
        days: 0,
        saved: 0,
        one_sided: 0,
        invalid: 0,
        failed: Vec::new(),
    };
    let mut date = from;
    loop {
        match fetch::backfill(&prepared.pool, &client, date, date).await {
            Ok(r) => {
                total.days += r.days;
                total.saved += r.saved;
                total.one_sided += r.one_sided;
                total.invalid += r.invalid;
                total.failed.extend(r.failed);
            }
            Err(SimError::FetchIncomplete(f)) => {
                total.days += 1;
                total.failed.extend(f);
            }
            Err(e) => {
                tracing::error!(
                    date = %date,
                    from = %from,
                    to = %to,
                    days = total.days,
                    saved = total.saved,
                    one_sided = total.one_sided,
                    invalid = total.invalid,
                    failed = ?total.failed,
                    error = %e,
                    "backfill aborted by a non-fetch error; counts above cover only the dates \
                     processed before this one"
                );
                return 1;
            }
        }
        if date == to {
            break;
        }
        match date.succ_opt() {
            Some(next) => date = next,
            None => break,
        }
    }

    if let Err(e) = print_backfill(out, json, &total) {
        return output_failed(e);
    }
    if !total.failed.is_empty() {
        tracing::error!(
            from = %from,
            to = %to,
            failed = ?total.failed,
            "failed to fetch some dates after retries: rerun the same range to \
             fill the gaps (already-saved dates are unaffected)"
        );
        return 1;
    }
    0
}

async fn run_benchmark(
    config_path: &Path,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    let prepared = match prepare(config_path).await {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (dataset, _, _) = match build_dataset(&prepared, from, to).await {
        Ok(d) => d,
        Err(code) => return code,
    };
    let benchmarks = compute_benchmarks(&dataset, &prepared.sim);
    match print_benchmark(out, &benchmarks, json) {
        Ok(()) => 0,
        Err(e) => output_failed(e),
    }
}

async fn run_run(
    config_path: &Path,
    script_path: PathBuf,
    params_raw: Option<String>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    run_run_with_hooks(
        config_path,
        script_path,
        params_raw,
        from,
        to,
        json,
        out,
        sweep::run_one,
        real_save_run(),
    )
    .await
}

#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn run_run_with_hooks(
    config_path: &Path,
    script_path: PathBuf,
    params_raw: Option<String>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    json: bool,
    out: &mut dyn std::io::Write,
    run_one_fn: RunOneFn,
    save_run_fn: SaveRunFn,
) -> i32 {
    let given = match parse_given_params(params_raw.as_deref()) {
        Ok(g) => g,
        Err(e) => {
            tracing::error!(error = %e, "invalid --params");
            return 1;
        }
    };
    let prepared = match prepare(config_path).await {
        Ok(p) => p,
        Err(code) => return code,
    };

    let host = ScriptHost::new(&prepared.sim);
    let compiled = match read_and_compile(&host, &script_path) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let params = match compiled.resolve_params(&given) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "invalid --params");
            return 1;
        }
    };

    let EvalInputs {
        dataset,
        from_secs,
        to_secs,
        bar_count,
        benchmarks,
    } = match load_eval_inputs(&prepared, from, to).await {
        Ok(i) => i,
        Err(code) => return code,
    };

    let script_name = match script_name_from_path(&script_path) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "failed to derive script name");
            return 1;
        }
    };
    let script_id = match store::register_script(
        &prepared.pool,
        &script_name,
        &compiled,
        "human",
        None,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to register script");
            return 1;
        }
    };
    let batch_id = match store::create_batch(
        &prepared.pool,
        &NewBatch {
            script_id,
            period_from: from_secs,
            period_to: to_secs,
            bar_count,
            total_runs: 1,
            config: run_batch_config_json(&prepared.sim),
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to create sim_batches row");
            return 1;
        }
    };

    let dataset = std::sync::Arc::new(dataset);
    let sim_cfg = prepared.sim.clone();
    let start = Instant::now();
    let run_result = tokio::task::spawn_blocking(move || {
        run_one_fn(&dataset, &host, &compiled, &params, &benchmarks, &sim_cfg)
    })
    .await;
    let record = match run_result {
        Ok(record) => record,
        Err(join_err) => {
            tracing::error!(
                batch_id = %batch_id,
                error = %join_err,
                "failed to run simulation: run_one panicked; the process did not crash, \
                 but no sim_runs row was saved"
            );
            return fail_batch_and_exit(&prepared.pool, batch_id).await;
        }
    };
    let save_result = save_run_fn(&prepared.pool, batch_id, &record).await;
    let elapsed = start.elapsed().as_secs_f64();

    if let Err(e) = save_result {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to save sim_runs row");
        return fail_batch_and_exit(&prepared.pool, batch_id).await;
    }
    if let Err(e) = store::finish_batch(&prepared.pool, batch_id, BatchStatus::Completed).await {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to mark sim_batches row as completed");
        return fail_batch_and_exit(&prepared.pool, batch_id).await;
    }

    if let Err(e) = print_run(out, &record, elapsed, script_id, batch_id, json) {
        return output_failed(e);
    }
    match record.status {
        RunStatus::Ok => 0,
        RunStatus::ScriptError { .. } => 1,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_sweep_cmd(
    config_path: &Path,
    script_path: PathBuf,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    max_runs: u32,
    seed: u64,
    jobs: Option<u64>,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    run_sweep_cmd_with_hooks(
        config_path,
        script_path,
        from,
        to,
        max_runs,
        seed,
        jobs,
        json,
        out,
        real_save_run(),
    )
    .await
}

#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn run_sweep_cmd_with_hooks(
    config_path: &Path,
    script_path: PathBuf,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    max_runs: u32,
    seed: u64,
    jobs: Option<u64>,
    json: bool,
    out: &mut dyn std::io::Write,
    save_run_fn: SaveRunFn,
) -> i32 {
    let jobs_override = match jobs.map(usize::try_from).transpose() {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(error = %e, "invalid --jobs: does not fit in usize on this platform");
            return 1;
        }
    };
    let prepared = match prepare(config_path).await {
        Ok(p) => p,
        Err(code) => return code,
    };

    let host = ScriptHost::new(&prepared.sim);
    let compiled = match read_and_compile(&host, &script_path) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let EvalInputs {
        dataset,
        from_secs,
        to_secs,
        bar_count,
        benchmarks,
    } = match load_eval_inputs(&prepared, from, to).await {
        Ok(i) => i,
        Err(code) => return code,
    };

    let max_runs_usize = max_runs as usize;
    let indices = match sweep::select_indices(&compiled.params, max_runs_usize, seed) {
        Ok(i) => i,
        Err(e) => {
            tracing::error!(error = %e, "failed to select sweep parameter combinations");
            return 1;
        }
    };
    let indices_len = indices.len();
    let total_runs = match i32::try_from(indices_len) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, indices_len, "invalid run count: does not fit sim_batches.total_runs as i32");
            return 1;
        }
    };

    let script_name = match script_name_from_path(&script_path) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "failed to derive script name");
            return 1;
        }
    };
    let script_id = match store::register_script(
        &prepared.pool,
        &script_name,
        &compiled,
        "human",
        None,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to register script");
            return 1;
        }
    };
    let batch_id = match store::create_batch(
        &prepared.pool,
        &NewBatch {
            script_id,
            period_from: from_secs,
            period_to: to_secs,
            bar_count,
            total_runs,
            config: sweep_batch_config_json(&prepared.sim, max_runs_usize, seed),
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to create sim_batches row");
            return 1;
        }
    };

    let requested_jobs = jobs_override.unwrap_or(prepared.sim.jobs);
    let available = match std::thread::available_parallelism() {
        Ok(n) => n.get(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to determine available_parallelism: assuming 1 for --jobs clamping"
            );
            1
        }
    };
    let jobs = effective_jobs(requested_jobs, indices_len, available);
    if jobs < requested_jobs {
        tracing::info!(
            requested_jobs,
            run_count = indices_len,
            available,
            jobs,
            "invalid --jobs: clamped to the minimum of the requested value, the run count, and \
             available_parallelism"
        );
    }
    let dataset = std::sync::Arc::new(dataset);
    let sim_cfg = prepared.sim.clone();
    let pool_for_saves = prepared.pool.clone();
    let rt_handle = tokio::runtime::Handle::current();

    let start = Instant::now();
    let sweep_result: Result<(), SimError> = tokio::task::spawn_blocking(move || {
        run_sweep(
            &dataset,
            &host,
            &compiled,
            &benchmarks,
            &sim_cfg,
            &indices,
            jobs,
            &mut |record: RunRecord| {
                rt_handle
                    .block_on(save_run_fn(&pool_for_saves, batch_id, &record))
                    .map(|_| ())
            },
        )
    })
    .await
    .unwrap_or_else(|join_err| {
        Err(SimError::BatchFailed(format!(
            "failed to run sweep: worker task panicked outside run_sweep's own panic handling: {join_err}"
        )))
    });
    let elapsed = start.elapsed().as_secs_f64();

    if let Err(e) = sweep_result {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to execute sweep");
        return fail_batch_and_exit(&prepared.pool, batch_id).await;
    }
    if let Err(e) = store::finish_batch(&prepared.pool, batch_id, BatchStatus::Completed).await {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to mark sim_batches row as completed");
        return fail_batch_and_exit(&prepared.pool, batch_id).await;
    }

    let top = match store::top_runs(&prepared.pool, batch_id, SWEEP_TOP_RUNS_LIMIT).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(batch_id = %batch_id, error = %e, "failed to load top runs for output");
            return 1;
        }
    };
    match print_sweep(out, &top, elapsed, script_id, batch_id, indices_len, json) {
        Ok(()) => 0,
        Err(e) => output_failed(e),
    }
}

#[cfg(test)]
mod tests;
