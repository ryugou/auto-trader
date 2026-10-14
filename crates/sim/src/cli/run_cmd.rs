use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use super::output::{output_failed, print_run};
use super::prepare::{
    EvalInputs, Prepared, RunOneFn, SaveRunFn, complete_batch, fail_batch_and_exit,
    load_eval_inputs, prepare, read_and_compile, real_save_run, register_script_and_batch,
    run_batch_config_json,
};
use crate::engine::RunStatus;
use crate::error::SimError;
use crate::script::{CompiledScript, ParamSet, ScriptHost};
use crate::sweep::{self, RunRecord};

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

pub(super) async fn run_run(
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

struct RunInputs {
    prepared: Prepared,
    host: ScriptHost,
    compiled: CompiledScript,
    params: ParamSet,
    eval_inputs: EvalInputs,
}

async fn load_run_inputs(
    config_path: &Path,
    script_path: &Path,
    params_raw: Option<&str>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<RunInputs, i32> {
    let given = parse_given_params(params_raw).map_err(|e| {
        tracing::error!(error = %e, "invalid --params");
        1
    })?;
    let prepared = prepare(config_path).await?;
    let host = ScriptHost::new(&prepared.sim);
    let compiled = read_and_compile(&host, script_path)?;
    let params = compiled.resolve_params(&given).map_err(|e| {
        tracing::error!(error = %e, "invalid --params");
        1
    })?;
    let eval_inputs = load_eval_inputs(&prepared, from, to).await?;
    Ok(RunInputs {
        prepared,
        host,
        compiled,
        params,
        eval_inputs,
    })
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
    let RunInputs {
        prepared,
        host,
        compiled,
        params,
        eval_inputs,
    } = match load_run_inputs(config_path, &script_path, params_raw.as_deref(), from, to).await {
        Ok(inputs) => inputs,
        Err(code) => return code,
    };

    let (script_id, batch_id) = match register_script_and_batch(
        &prepared.pool,
        &script_path,
        &compiled,
        &eval_inputs,
        1,
        run_batch_config_json(&prepared.sim),
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

    let sim_cfg = prepared.sim.clone();
    let dataset = std::sync::Arc::new(dataset);
    let (record, elapsed) =
        match execute_and_save_run(&prepared.pool, batch_id, &save_run_fn, move || {
            run_one_fn(&dataset, &host, &compiled, &params, &benchmarks, &sim_cfg)
        })
        .await
        {
            Ok(result) => result,
            Err(code) => return code,
        };

    if let Err(code) = complete_batch(&prepared.pool, batch_id).await {
        return code;
    }

    if let Err(e) = print_run(out, &record, elapsed, script_id, batch_id, json) {
        return output_failed(e);
    }
    match record.status {
        RunStatus::Ok => 0,
        RunStatus::ScriptError { .. } => 1,
    }
}

async fn resolve_run_result(
    pool: &PgPool,
    batch_id: Uuid,
    run_result: Result<Result<RunRecord, SimError>, tokio::task::JoinError>,
) -> Result<RunRecord, i32> {
    match run_result {
        Ok(Ok(record)) => Ok(record),
        Ok(Err(e)) => {
            tracing::error!(batch_id = %batch_id, error = %e, "failed to run simulation");
            Err(fail_batch_and_exit(pool, batch_id).await)
        }
        Err(join_err) => {
            tracing::error!(
                batch_id = %batch_id,
                error = %join_err,
                "failed to run simulation: run_one panicked; the process did not crash, \
                 but no sim_runs row was saved"
            );
            Err(fail_batch_and_exit(pool, batch_id).await)
        }
    }
}

async fn execute_and_save_run(
    pool: &PgPool,
    batch_id: Uuid,
    save_run_fn: &SaveRunFn,
    run_one: impl FnOnce() -> Result<RunRecord, SimError> + Send + 'static,
) -> Result<(RunRecord, f64), i32> {
    let start = Instant::now();
    let run_result = tokio::task::spawn_blocking(run_one).await;
    let record = resolve_run_result(pool, batch_id, run_result).await?;
    let save_result = save_run_fn(pool, batch_id, &record).await;
    let elapsed = start.elapsed().as_secs_f64();

    if let Err(e) = save_result {
        tracing::error!(batch_id = %batch_id, error = %e, "failed to save sim_runs row");
        return Err(fail_batch_and_exit(pool, batch_id).await);
    }
    Ok((record, elapsed))
}
