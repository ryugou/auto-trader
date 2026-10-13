use std::path::Path;

use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use crate::benchmark::{self, Benchmark};
use crate::config::{self, SimConfig};
use crate::data;
use crate::error::SimError;
use crate::script::{CompiledScript, ParamSet, ScriptHost};
use crate::series::{self, Dataset};
use crate::store::{self, BatchStatus, NewBatch};
use crate::sweep::RunRecord;
use crate::types::Bar;

pub fn resolve_period(
    all_bars: &[Bar],
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    warmup_bars: usize,
) -> Result<(i64, i64), SimError> {
    let Some(last_bar) = all_bars.last() else {
        return Err(SimError::Args(
            "invalid sim_candles: no bars are loaded; run `backfill` first".to_string(),
        ));
    };

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
            let last_date = chrono::DateTime::<chrono::Utc>::from_timestamp(last_bar.open_time, 0)
                .ok_or_else(|| {
                    SimError::Args(format!(
                        "invalid sim_candles.open_time: not a valid utc timestamp (open_time={})",
                        last_bar.open_time
                    ))
                })?
                .date_naive();
            let next_date = last_date.succ_opt().ok_or_else(|| {
                SimError::Args(format!(
                    "invalid sim_candles.open_time: date arithmetic overflow computing the day \
                     after {last_date} (near NaiveDate::MAX)"
                ))
            })?;
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

pub(super) struct Prepared {
    pub(super) pool: PgPool,
    pub(super) sim: SimConfig,
}

pub(super) async fn load_config_and_connect(
    config_path: &Path,
) -> Result<(PgPool, SimConfig), i32> {
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
    Ok((pool, settings.sim))
}

pub(super) async fn prepare(config_path: &Path) -> Result<Prepared, i32> {
    let (pool, sim) = load_config_and_connect(config_path).await?;
    data::ensure_tables(&pool).await.map_err(|e| {
        tracing::error!(error = %e, "invalid database: required sim_* tables are missing");
        1
    })?;
    Ok(Prepared { pool, sim })
}

pub(super) async fn fail_batch_and_exit(pool: &PgPool, batch_id: Uuid) -> i32 {
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

pub(super) fn read_and_compile(
    host: &ScriptHost,
    script_path: &Path,
) -> Result<CompiledScript, i32> {
    let source = std::fs::read_to_string(script_path).map_err(|e| {
        tracing::error!(script = %script_path.display(), error = %e, "failed to read script file");
        1
    })?;
    host.compile(&source).map_err(|e| {
        tracing::error!(script = %script_path.display(), error = %e, "invalid script: failed registration validation");
        1
    })
}

pub(super) async fn build_dataset(
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

pub(super) fn compute_benchmarks(dataset: &Dataset, sim: &SimConfig) -> Vec<Benchmark> {
    sim.thetas_pips
        .iter()
        .map(|&theta| benchmark::compute(dataset.eval_bars(), theta))
        .collect()
}

pub(super) struct EvalInputs {
    pub(super) dataset: Dataset,
    pub(super) from_secs: i64,
    pub(super) to_secs: i64,
    pub(super) bar_count: i32,
    pub(super) benchmarks: Vec<Benchmark>,
}

pub(super) async fn load_eval_inputs(
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

pub(super) fn run_batch_config_json(sim: &SimConfig) -> serde_json::Value {
    serde_json::json!({
        "warmup_bars": sim.warmup_bars,
        "protective_stop_pips": sim.protective_stop_pips,
        "thetas_pips": sim.thetas_pips,
        "max_operations_per_bar": sim.max_operations_per_bar,
        "max_operations_per_run": sim.max_operations_per_run,
    })
}

pub(super) async fn register_script_and_batch(
    pool: &PgPool,
    script_path: &Path,
    compiled: &CompiledScript,
    eval_inputs: &EvalInputs,
    total_runs: i32,
    config: serde_json::Value,
) -> Result<(Uuid, Uuid), i32> {
    let script_name = match script_name_from_path(script_path) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "failed to derive script name");
            return Err(1);
        }
    };
    let script_id = match store::register_script(pool, &script_name, compiled, "human", None).await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to register script");
            return Err(1);
        }
    };
    let batch_id = match store::create_batch(
        pool,
        &NewBatch {
            script_id,
            period_from: eval_inputs.from_secs,
            period_to: eval_inputs.to_secs,
            bar_count: eval_inputs.bar_count,
            total_runs,
            config,
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to create sim_batches row");
            return Err(1);
        }
    };
    Ok((script_id, batch_id))
}

pub(super) async fn complete_batch(pool: &PgPool, batch_id: Uuid) -> Result<(), i32> {
    if let Err(e) = store::finish_batch(pool, batch_id, BatchStatus::Completed).await {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to mark sim_batches row as completed");
        return Err(fail_batch_and_exit(pool, batch_id).await);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
