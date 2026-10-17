use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, mpsc};

use rand::SeedableRng;
use rand::seq::index as rand_index;
use rand_chacha::ChaCha8Rng;

use crate::benchmark::Benchmark;
use crate::config::SimConfig;
use crate::engine::{RunStatus, simulate};
use crate::error::SimError;
use crate::eval::{self, Metrics};
use crate::script::{
    CompiledScript, ParamKind, ParamSet, ParamSpec, ParamValue, ScriptHost, ScriptRun,
};
use crate::series::Dataset;
use crate::types::FLOAT_GRID_TOLERANCE;

struct WorkQueue {
    next: usize,
    cancelled: bool,
}

impl WorkQueue {
    fn claim_next_unless_cancelled(&mut self, len: usize) -> Option<usize> {
        if self.cancelled || self.next >= len {
            return None;
        }
        let i = self.next;
        self.next += 1;
        Some(i)
    }
}

const MAX_TOTAL_COMBINATIONS: u128 = 1u128 << 63;
const MAX_FLOAT_K_ADJUSTMENTS: usize = 4;

fn widen_i64_span_to_i128(min: i64, value: i64) -> i128 {
    value as i128 - min as i128
}

/// Returns the number of values on the grid that runs from `min` to `max` in steps of `step`, ends included.
///
/// # Panics
///
/// Panics if the kind is `Int` and `step` truncated to an integer is zero (division by zero).
pub fn candidate_count(spec: &ParamSpec) -> u64 {
    match spec.kind {
        ParamKind::Int => {
            let min = spec.min as i64;
            let max = spec.max as i64;
            let step = spec.step as i64;
            let span = widen_i64_span_to_i128(min, max);
            ((span / step as i128) as u64).saturating_add(1)
        }
        ParamKind::Float => {
            let fits = |k: u64| spec.min + k as f64 * spec.step <= spec.max + FLOAT_GRID_TOLERANCE;
            let mut k = ((spec.max - spec.min) / spec.step).floor() as u64;
            for _ in 0..MAX_FLOAT_K_ADJUSTMENTS {
                if k > 0 && !fits(k) {
                    k -= 1;
                } else {
                    break;
                }
            }
            for _ in 0..MAX_FLOAT_K_ADJUSTMENTS {
                match k.checked_add(1) {
                    Some(next) if fits(next) => k = next,
                    _ => break,
                }
            }
            k.saturating_add(1)
        }
    }
}

fn too_many_combinations_error() -> SimError {
    SimError::Args(format!(
        "invalid sweep: more than {MAX_TOTAL_COMBINATIONS} parameter combinations; narrow the \
         parameter ranges or use a coarser step to reduce the candidate count"
    ))
}

/// Returns the number of parameter combinations, which is the product of the candidate counts of `specs`.
///
/// # Errors
///
/// - Returns an error if the product exceeds 2^63.
///
/// # Panics
///
/// Panics if a spec of kind `Int` has a `step` that truncated to an integer is zero (division by zero).
pub fn total_combinations(specs: &[ParamSpec]) -> Result<u128, SimError> {
    let mut total: u128 = 1;
    for spec in specs {
        let count = candidate_count(spec) as u128;
        total = total
            .checked_mul(count)
            .filter(|&t| t <= MAX_TOTAL_COMBINATIONS)
            .ok_or_else(too_many_combinations_error)?;
    }
    Ok(total)
}

fn default_k(spec: &ParamSpec) -> u64 {
    match spec.kind {
        ParamKind::Int => {
            let min = spec.min as i64;
            let step = spec.step as i64;
            let default = spec.default as i64;
            (widen_i64_span_to_i128(min, default) / step as i128) as u64
        }
        ParamKind::Float => (((spec.default - spec.min) / spec.step).round()) as u64,
    }
}

fn param_value_at(spec: &ParamSpec, k: u64) -> ParamValue {
    match spec.kind {
        ParamKind::Int => {
            let min = spec.min as i64;
            let step = spec.step as i64;
            let value = min as i128 + k as i128 * step as i128;
            ParamValue::Int(value as i64)
        }
        ParamKind::Float => ParamValue::Float(spec.min + k as f64 * spec.step),
    }
}

/// Returns the index of the combination that holds the default value of every parameter.
///
/// # Errors
///
/// - Returns an error if the number of combinations exceeds 2^63.
///
/// # Panics
///
/// Panics if a spec of kind `Int` has a `step` that truncated to an integer is zero (division by zero).
pub fn default_index(specs: &[ParamSpec]) -> Result<u128, SimError> {
    total_combinations(specs)?;
    let mut result: u128 = 0;
    for spec in specs {
        let radix = candidate_count(spec) as u128;
        result = result * radix + default_k(spec) as u128;
    }
    Ok(result)
}

/// Returns the parameter set at `index` in the grid of `specs`, where the last spec varies fastest.
///
/// # Panics
///
/// Panics if a spec of kind `Int` has a `step` that truncated to an integer is zero (division by zero).
pub fn combination_at(specs: &[ParamSpec], index: u128) -> ParamSet {
    let mut remaining = index;
    let mut ks = vec![0u64; specs.len()];
    for i in (0..specs.len()).rev() {
        let radix = candidate_count(&specs[i]) as u128;
        ks[i] = (remaining % radix) as u64;
        remaining /= radix;
    }
    specs
        .iter()
        .zip(ks)
        .map(|(spec, k)| (spec.name.clone(), param_value_at(spec, k)))
        .collect()
}

/// Returns at most `max_runs` combination indices, with the default combination first. All combinations are returned when they fit in `max_runs`. Otherwise the rest is a random sample drawn from `seed`.
///
/// # Errors
///
/// - Returns an error if the number of combinations exceeds 2^63.
/// - Returns an error if the number of combinations does not fit `usize`.
///
/// # Panics
///
/// Panics if a spec of kind `Int` has a `step` that truncated to an integer is zero (division by zero).
pub fn select_indices(
    specs: &[ParamSpec],
    max_runs: usize,
    seed: u64,
) -> Result<Vec<u128>, SimError> {
    let total = total_combinations(specs)?;
    let default_idx = default_index(specs)?;

    if max_runs == 0 {
        return Ok(Vec::new());
    }

    if total <= max_runs as u128 {
        let mut indices = Vec::with_capacity(total as usize);
        indices.push(default_idx);
        for i in 0..total {
            if i != default_idx {
                indices.push(i);
            }
        }
        return Ok(indices);
    }

    let pool_len = total - 1;
    let take = max_runs - 1;
    let pool_len_usize: usize = pool_len.try_into().map_err(|e| {
        SimError::Other(anyhow::anyhow!(
            "invalid platform: total_combinations is capped at 2^63, which must fit in usize on \
             our 64-bit targets, but converting {pool_len} to usize failed: {e}"
        ))
    })?;

    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let sampled = rand_index::sample(&mut rng, pool_len_usize, take);

    let mut indices = Vec::with_capacity(max_runs);
    indices.push(default_idx);
    for j in sampled.into_iter() {
        let j = j as u128;
        let actual = if j < default_idx { j } else { j + 1 };
        indices.push(actual);
    }
    Ok(indices)
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Result of one run of a script.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    /// Parameters of the run.
    pub params: ParamSet,
    /// Whether the run finished or stopped on a script error.
    pub status: RunStatus,
    /// Metrics of the run, or `None` when the run stopped on a script error.
    pub metrics: Option<Metrics>,
}

/// Runs the script once with `params` and evaluates the result against `benchmarks`.
///
/// # Errors
///
/// - Returns an error if the simulation fails fatally.
///
/// # Panics
///
/// Panics if a benchmark of `benchmarks` has more labels than the evaluation bars, or a leg of it lies beyond them (see `eval::evaluate`).
pub fn run_one(
    dataset: &Arc<Dataset>,
    host: &ScriptHost,
    script: &CompiledScript,
    params: &ParamSet,
    benchmarks: &[Benchmark],
    cfg: &SimConfig,
) -> Result<RunRecord, SimError> {
    let mut run = ScriptRun::new(host, script, params, dataset.clone(), cfg);
    let outcome = simulate(dataset, &mut run, cfg)?;
    let metrics = match &outcome.status {
        RunStatus::Ok => Some(eval::evaluate(dataset.eval_bars(), &outcome, benchmarks)),
        RunStatus::ScriptError { .. } => None,
    };
    Ok(RunRecord {
        params: params.clone(),
        status: outcome.status,
        metrics,
    })
}

fn channel_capacity(jobs: usize) -> usize {
    jobs
}

/// Runs the combinations at `indices` on `jobs` threads and passes each result to `on_result`.
///
/// # Errors
///
/// - Returns the first error that `on_result` returns, and the remaining combinations are not started.
/// - Returns the first fatal error of a run.
/// - Returns an error if a worker panicked, naming the combination index.
#[allow(clippy::too_many_arguments)]
pub fn run_sweep(
    dataset: &Arc<Dataset>,
    host: &ScriptHost,
    script: &CompiledScript,
    benchmarks: &[Benchmark],
    cfg: &SimConfig,
    indices: &[u128],
    jobs: usize,
    on_result: &mut (dyn FnMut(RunRecord) -> Result<(), SimError> + Send),
) -> Result<(), SimError> {
    run_sweep_with_hook(
        dataset,
        host,
        script,
        benchmarks,
        cfg,
        indices,
        jobs,
        on_result,
        &|| {},
    )
}

fn run_one_combination_catching_panics(
    dataset: &Arc<Dataset>,
    host: &ScriptHost,
    script: &CompiledScript,
    benchmarks: &[Benchmark],
    cfg: &SimConfig,
    idx: u128,
) -> Result<Result<RunRecord, SimError>, Box<dyn std::any::Any + Send>> {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let params = combination_at(&script.params, idx);
        run_one(dataset, host, script, &params, benchmarks, cfg)
    }))
}

fn drain_sweep_results(
    rx: &mpsc::Receiver<RunRecord>,
    queue: &Mutex<WorkQueue>,
    total: usize,
    on_result: &mut (dyn FnMut(RunRecord) -> Result<(), SimError> + Send),
) -> Option<SimError> {
    let mut received = 0usize;
    let mut final_err: Option<SimError> = None;
    while received < total {
        match rx.recv() {
            Ok(record) => {
                received += 1;
                if final_err.is_some() {
                    continue;
                }
                if let Err(e) = on_result(record) {
                    final_err = Some(e);
                    queue.lock().unwrap_or_else(|e| e.into_inner()).cancelled = true;
                }
            }
            Err(_) => {
                break;
            }
        }
    }
    final_err
}

fn sweep_panic_result(first_panic: &Mutex<Option<(u128, String)>>) -> Result<(), SimError> {
    let Some((idx, message)) = first_panic.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return Ok(());
    };
    Err(SimError::BatchFailed(format!(
        "failed to run sweep worker at combination index {idx}: {message}"
    )))
}

#[allow(clippy::too_many_arguments)]
fn run_sweep_with_hook(
    dataset: &Arc<Dataset>,
    host: &ScriptHost,
    script: &CompiledScript,
    benchmarks: &[Benchmark],
    cfg: &SimConfig,
    indices: &[u128],
    jobs: usize,
    on_result: &mut (dyn FnMut(RunRecord) -> Result<(), SimError> + Send),
    on_worker_complete: &(dyn Fn() + Sync),
) -> Result<(), SimError> {
    if indices.is_empty() {
        return Ok(());
    }
    let jobs = jobs.max(1);

    let queue = Mutex::new(WorkQueue {
        next: 0,
        cancelled: false,
    });
    let first_panic: Mutex<Option<(u128, String)>> = Mutex::new(None);
    let first_fatal: Mutex<Option<SimError>> = Mutex::new(None);
    let (tx, rx) = mpsc::sync_channel::<RunRecord>(channel_capacity(jobs));

    std::thread::scope(|scope| {
        let rx = rx;
        for _ in 0..jobs {
            let tx = tx.clone();
            let queue = &queue;
            let first_panic = &first_panic;
            let first_fatal = &first_fatal;
            scope.spawn(move || {
                loop {
                    let i = {
                        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                        match q.claim_next_unless_cancelled(indices.len()) {
                            Some(i) => i,
                            None => break,
                        }
                    };
                    let idx = indices[i];
                    let result = run_one_combination_catching_panics(
                        dataset, host, script, benchmarks, cfg, idx,
                    );
                    match result {
                        Ok(Ok(record)) => {
                            on_worker_complete();
                            if tx.send(record).is_err() {
                                break;
                            }
                        }
                        Ok(Err(sim_error)) => {
                            let mut slot = first_fatal.lock().unwrap_or_else(|e| e.into_inner());
                            if slot.is_none() {
                                *slot = Some(sim_error);
                            }
                            drop(slot);
                            queue.lock().unwrap_or_else(|e| e.into_inner()).cancelled = true;
                            break;
                        }
                        Err(payload) => {
                            let message = panic_message(payload.as_ref());
                            let mut slot = first_panic.lock().unwrap_or_else(|e| e.into_inner());
                            if slot.is_none() {
                                *slot = Some((idx, message));
                            }
                            drop(slot);
                            queue.lock().unwrap_or_else(|e| e.into_inner()).cancelled = true;
                            break;
                        }
                    }
                }
            });
        }
        drop(tx);

        if let Some(e) = drain_sweep_results(&rx, &queue, indices.len(), on_result) {
            return Err(e);
        }
        if let Some(e) = first_fatal.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return Err(e);
        }
        sweep_panic_result(&first_panic)
    })
}

#[cfg(test)]
mod tests;
