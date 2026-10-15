use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::NaiveDate;

use crate::config::SimConfig;
use crate::error::SimError;
use crate::script::ScriptHost;
use crate::store::{self, BatchStatus, NewBatch};
use crate::sweep::{self, RunRecord, run_sweep};

mod args;
mod backfill;
mod benchmark;
mod dispatch;
mod migrate;
mod output;
mod prepare;
mod run_cmd;

pub use args::{Cli, Command};
pub use dispatch::{run, run_args, run_with_config_path, run_with_config_path_and_output};
pub use prepare::{RunOneFn, SaveRunFn, real_save_run, resolve_period};
pub use run_cmd::run_run_with_hooks;

use output::{output_failed, print_sweep};
use prepare::{
    EvalInputs, fail_batch_and_exit, load_eval_inputs, prepare, read_and_compile,
    run_batch_config_json, script_name_from_path,
};

const SWEEP_TOP_RUNS_LIMIT: i64 = 10;

fn sweep_batch_config_json(sim: &SimConfig, max_runs: usize, seed: u64) -> serde_json::Value {
    let mut value = run_batch_config_json(sim);
    value["max_runs"] = serde_json::json!(max_runs);
    value["seed"] = serde_json::json!(seed);
    value
}

fn effective_jobs(requested: usize, run_count: usize, available: usize) -> usize {
    requested.min(run_count).min(available).max(1)
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
