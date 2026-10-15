use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use super::output::{output_failed, print_sweep};
use super::prepare::{
    EvalInputs, Prepared, SaveRunFn, complete_batch, fail_batch_and_exit, load_eval_inputs,
    prepare, read_and_compile, real_save_run, register_script_and_batch, run_batch_config_json,
};
use crate::config::SimConfig;
use crate::error::SimError;
use crate::script::{CompiledScript, ScriptHost};
use crate::store;
use crate::sweep::{self, RunRecord, run_sweep};

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
pub(super) async fn run_sweep_cmd(
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

struct SweepInputs {
    prepared: Prepared,
    host: ScriptHost,
    compiled: CompiledScript,
    eval_inputs: EvalInputs,
    max_runs_usize: usize,
    indices: Vec<u128>,
    total_runs: i32,
}

async fn load_sweep_inputs(
    config_path: &Path,
    script_path: &Path,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    max_runs: u32,
    seed: u64,
) -> Result<SweepInputs, i32> {
    let prepared = prepare(config_path).await?;
    let host = ScriptHost::new(&prepared.sim);
    let compiled = read_and_compile(&host, script_path)?;
    let eval_inputs = load_eval_inputs(&prepared, from, to).await?;
    let (max_runs_usize, indices, total_runs) = select_sweep_indices(&compiled, max_runs, seed)?;
    Ok(SweepInputs {
        prepared,
        host,
        compiled,
        eval_inputs,
        max_runs_usize,
        indices,
        total_runs,
    })
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

    let SweepInputs {
        prepared,
        host,
        compiled,
        eval_inputs,
        max_runs_usize,
        indices,
        total_runs,
    } = match load_sweep_inputs(config_path, &script_path, from, to, max_runs, seed).await {
        Ok(inputs) => inputs,
        Err(code) => return code,
    };
    let indices_len = indices.len();

    let (script_id, batch_id) = match register_script_and_batch(
        &prepared.pool,
        &script_path,
        &compiled,
        &eval_inputs,
        total_runs,
        sweep_batch_config_json(&prepared.sim, max_runs_usize, seed),
    )
    .await
    {
        Ok(ids) => ids,
        Err(code) => return code,
    };
    let EvalInputs {
        dataset,
        benchmarks,
        ..
    } = eval_inputs;

    let jobs = compute_effective_jobs(jobs_override, prepared.sim.jobs, indices_len);

    let dataset = std::sync::Arc::new(dataset);
    let sim_cfg = prepared.sim.clone();
    let pool_for_saves = prepared.pool.clone();
    let rt_handle = tokio::runtime::Handle::current();

    let elapsed = match execute_sweep(&prepared.pool, batch_id, move || {
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
    {
        Ok(elapsed) => elapsed,
        Err(code) => return code,
    };

    if let Err(code) = complete_batch(&prepared.pool, batch_id).await {
        return code;
    }

    write_sweep_result(
        &prepared.pool,
        batch_id,
        out,
        elapsed,
        script_id,
        indices_len,
        json,
    )
    .await
}

fn select_sweep_indices(
    compiled: &CompiledScript,
    max_runs: u32,
    seed: u64,
) -> Result<(usize, Vec<u128>, i32), i32> {
    let max_runs_usize = max_runs as usize;
    let indices = match sweep::select_indices(&compiled.params, max_runs_usize, seed) {
        Ok(i) => i,
        Err(e) => {
            tracing::error!(error = %e, "failed to select sweep parameter combinations");
            return Err(1);
        }
    };
    let indices_len = indices.len();
    let total_runs = match i32::try_from(indices_len) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, indices_len, "invalid run count: does not fit sim_batches.total_runs as i32");
            return Err(1);
        }
    };
    Ok((max_runs_usize, indices, total_runs))
}

fn compute_effective_jobs(
    jobs_override: Option<usize>,
    default_jobs: usize,
    run_count: usize,
) -> usize {
    let requested_jobs = jobs_override.unwrap_or(default_jobs);
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
    let jobs = effective_jobs(requested_jobs, run_count, available);
    if jobs < requested_jobs {
        tracing::info!(
            requested_jobs,
            run_count,
            available,
            jobs,
            "invalid --jobs: clamped to the minimum of the requested value, the run count, and \
             available_parallelism"
        );
    }
    jobs
}

async fn execute_sweep(
    pool: &PgPool,
    batch_id: Uuid,
    run_sweep_fn: impl FnOnce() -> Result<(), SimError> + Send + 'static,
) -> Result<f64, i32> {
    let start = Instant::now();
    let sweep_result: Result<(), SimError> = tokio::task::spawn_blocking(run_sweep_fn)
        .await
        .unwrap_or_else(|join_err| {
            Err(SimError::BatchFailed(format!(
                "failed to run sweep: worker task panicked outside run_sweep's own panic handling: {join_err}"
            )))
        });
    let elapsed = start.elapsed().as_secs_f64();

    if let Err(e) = sweep_result {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to execute sweep");
        return Err(fail_batch_and_exit(pool, batch_id).await);
    }
    Ok(elapsed)
}

async fn write_sweep_result(
    pool: &PgPool,
    batch_id: Uuid,
    out: &mut dyn std::io::Write,
    elapsed: f64,
    script_id: Uuid,
    total_runs: usize,
    json: bool,
) -> i32 {
    let top = match store::top_runs(pool, batch_id, SWEEP_TOP_RUNS_LIMIT).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(batch_id = %batch_id, error = %e, "failed to load top runs for output");
            return 1;
        }
    };
    match print_sweep(out, &top, elapsed, script_id, batch_id, total_runs, json) {
        Ok(()) => 0,
        Err(e) => output_failed(e),
    }
}

#[cfg(test)]
mod tests;
