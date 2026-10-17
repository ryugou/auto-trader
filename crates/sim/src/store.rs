use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::data::{epoch_secs_to_datetime, format_epoch_secs_rfc3339};
use crate::engine::RunStatus;
use crate::error::SimError;
use crate::eval::Metrics;
use crate::script::{CompiledScript, ParamSet, ParamValue};
use crate::sweep::RunRecord;

fn rfc3339(open_time: i64) -> Result<String, SimError> {
    format_epoch_secs_rfc3339(open_time).ok_or_else(|| {
        SimError::Other(anyhow::anyhow!(
            "invalid open_time: {open_time} is not a representable UTC instant"
        ))
    })
}

/// Converts a parameter set to a JSON object that maps each parameter name to its value.
pub fn params_json(params: &ParamSet) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = params
        .iter()
        .map(|(name, value)| {
            let json_value = match value {
                ParamValue::Int(n) => serde_json::Value::from(*n),
                ParamValue::Float(f) => serde_json::Value::from(*f),
            };
            (name.clone(), json_value)
        })
        .collect();
    serde_json::Value::Object(map)
}

/// Stores the script in `sim_scripts` and returns its id, or returns the id of the stored script with the same source hash without inserting.
///
/// # Errors
///
/// - Returns an error if `origin` is `human` and `parent_id` is given.
/// - Returns an error if the insert or the lookup of the existing script fails.
pub async fn register_script(
    pool: &PgPool,
    name: &str,
    script: &CompiledScript,
    origin: &str,
    parent_id: Option<Uuid>,
) -> Result<Uuid, SimError> {
    if origin == "human" && parent_id.is_some() {
        return Err(SimError::Args(format!(
            "invalid parent_id: must be none when origin is human (parent_id={parent_id:?})"
        )));
    }
    let candidate_id = Uuid::new_v4();
    let inserted: Option<Uuid> = sqlx::query_scalar(
        r#"
        INSERT INTO sim_scripts (id, name, source, source_sha256, parent_id, origin)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (source_sha256) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(candidate_id)
    .bind(name)
    .bind(&script.source)
    .bind(&script.sha256)
    .bind(parent_id)
    .bind(origin)
    .fetch_optional(pool)
    .await?;

    match inserted {
        Some(id) => Ok(id),
        None => {
            let existing_id: Uuid =
                sqlx::query_scalar("SELECT id FROM sim_scripts WHERE source_sha256 = $1")
                    .bind(&script.sha256)
                    .fetch_one(pool)
                    .await?;
            Ok(existing_id)
        }
    }
}

/// Batch of runs to be inserted into `sim_batches`.
#[derive(Debug, Clone)]
pub struct NewBatch {
    /// Id of the script that the runs execute.
    pub script_id: Uuid,
    /// Start of the evaluation period in Unix seconds.
    pub period_from: i64,
    /// End of the evaluation period in Unix seconds, exclusive.
    pub period_to: i64,
    /// Number of evaluation bars.
    pub bar_count: i32,
    /// Number of runs of the batch.
    pub total_runs: i32,
    /// Settings of the simulation, as JSON.
    pub config: serde_json::Value,
}

/// Status of a batch in `sim_batches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchStatus {
    /// The runs are in progress.
    Running,
    /// The batch finished.
    Completed,
    /// The batch failed.
    Failed,
}

impl BatchStatus {
    fn as_sql(self) -> &'static str {
        match self {
            BatchStatus::Running => "running",
            BatchStatus::Completed => "completed",
            BatchStatus::Failed => "failed",
        }
    }
}

/// Inserts the batch into `sim_batches` with the status `running` and returns its id.
///
/// # Errors
///
/// - Returns an error if `period_from` or `period_to` is not a representable UTC instant.
/// - Returns an error if the insert fails.
pub async fn create_batch(pool: &PgPool, batch: &NewBatch) -> Result<Uuid, SimError> {
    let id = Uuid::new_v4();
    let period_from = epoch_secs_to_datetime(batch.period_from, "create_batch period_from")?;
    let period_to = epoch_secs_to_datetime(batch.period_to, "create_batch period_to")?;
    sqlx::query(
        r#"
        INSERT INTO sim_batches
            (id, script_id, status, period_from, period_to, bar_count, total_runs, config)
        VALUES ($1, $2, 'running', $3, $4, $5, $6, $7)
        "#,
    )
    .bind(id)
    .bind(batch.script_id)
    .bind(period_from)
    .bind(period_to)
    .bind(batch.bar_count)
    .bind(batch.total_runs)
    .bind(&batch.config)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Sets a running batch to `status` and records the finish time.
///
/// # Errors
///
/// - Returns an error if `status` is `Running`.
/// - Returns an error if the update fails.
/// - Returns an error if the batch does not exist or is not running.
pub async fn finish_batch(
    pool: &PgPool,
    batch_id: Uuid,
    status: BatchStatus,
) -> Result<(), SimError> {
    if status == BatchStatus::Running {
        return Err(SimError::Args(format!(
            "invalid batch status transition: batch {batch_id} to 'running' is not allowed \
             (only running -> completed/failed is allowed)"
        )));
    }
    let result = sqlx::query(
        "UPDATE sim_batches SET status = $1, finished_at = $2 WHERE id = $3 AND status = 'running'",
    )
    .bind(status.as_sql())
    .bind(Utc::now())
    .bind(batch_id)
    .execute(pool)
    .await?;
    if result.rows_affected() != 1 {
        return Err(SimError::Args(format!(
            "invalid batch status transition: batch {batch_id} could not be set to '{}' \
             (it does not exist or is not in 'running' state)",
            status.as_sql()
        )));
    }
    Ok(())
}

async fn insert_ok_run(
    pool: &PgPool,
    id: Uuid,
    batch_id: Uuid,
    params: &serde_json::Value,
    metrics: &Metrics,
) -> Result<(), SimError> {
    let trade_count = i32::try_from(metrics.trade_count).map_err(|e| {
        SimError::Other(anyhow::anyhow!(
            "invalid run record: trade_count {} does not fit sim_runs.trade_count as \
             i32: {e} (batch_id={batch_id}, params={params})",
            metrics.trade_count
        ))
    })?;
    let protective_stop_count = i32::try_from(metrics.protective_stop_count).map_err(|e| {
        SimError::Other(anyhow::anyhow!(
            "invalid run record: protective_stop_count {} does not fit \
             sim_runs.protective_stop_count as i32: {e} \
             (batch_id={batch_id}, params={params})",
            metrics.protective_stop_count
        ))
    })?;
    sqlx::query(
        r#"
        INSERT INTO sim_runs
            (id, batch_id, params, status, error,
             total_pips, trade_count, win_rate, max_drawdown_pips, time_in_market,
             protective_stop_count, metrics)
        VALUES ($1, $2, $3, 'ok', NULL, $4, $5, $6, $7, $8, $9, $10)
        "#,
    )
    .bind(id)
    .bind(batch_id)
    .bind(params)
    .bind(metrics.total_pips)
    .bind(trade_count)
    .bind(metrics.win_rate)
    .bind(metrics.max_drawdown_pips)
    .bind(metrics.time_in_market)
    .bind(protective_stop_count)
    .bind(metrics.metrics_json())
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_script_error_run(
    pool: &PgPool,
    id: Uuid,
    batch_id: Uuid,
    params: &serde_json::Value,
    open_time: i64,
    message: &str,
) -> Result<(), SimError> {
    let open_time = rfc3339(open_time).map_err(|e| {
        SimError::Other(anyhow::anyhow!(
            "failed to format script_error open_time for sim_runs.error: {e} \
             (batch_id={batch_id}, params={params})"
        ))
    })?;
    let error = format!("{open_time} {message}");
    sqlx::query(
        r#"
        INSERT INTO sim_runs
            (id, batch_id, params, status, error,
             total_pips, trade_count, win_rate, max_drawdown_pips, time_in_market,
             protective_stop_count, metrics)
        VALUES ($1, $2, $3, 'script_error', $4, NULL, NULL, NULL, NULL, NULL, NULL, NULL)
        "#,
    )
    .bind(id)
    .bind(batch_id)
    .bind(params)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

/// Inserts the run record into `sim_runs` for the batch and returns the id of the new row.
///
/// A record whose status is `RunStatus::Ok` is stored with its metrics, and one whose status is `RunStatus::ScriptError` is stored with the failure time and message.
///
/// # Errors
///
/// - Returns an error if the status is `Ok` and the metrics are missing, or the status is `ScriptError` and the metrics are present.
/// - Returns an error if the trade count or the protective stop count of the metrics does not fit `i32`.
/// - Returns an error if the failure time of a script error is not a representable UTC instant.
/// - Returns an error if the insert fails.
pub async fn save_run(pool: &PgPool, batch_id: Uuid, record: &RunRecord) -> Result<Uuid, SimError> {
    let id = Uuid::new_v4();
    let params = params_json(&record.params);

    match (&record.status, &record.metrics) {
        (RunStatus::Ok, None) => {
            return Err(SimError::Other(anyhow::anyhow!(
                "invalid run record: status is Ok but metrics is None \
                 (batch_id={batch_id}, params={params})"
            )));
        }
        (RunStatus::ScriptError { .. }, Some(_)) => {
            return Err(SimError::Other(anyhow::anyhow!(
                "invalid run record: status is ScriptError but metrics is Some \
                 (batch_id={batch_id}, params={params})"
            )));
        }
        (RunStatus::Ok, Some(metrics)) => {
            insert_ok_run(pool, id, batch_id, &params, metrics).await?;
        }
        (RunStatus::ScriptError { open_time, message }, None) => {
            insert_script_error_run(pool, id, batch_id, &params, *open_time, message).await?;
        }
    }

    Ok(id)
}

/// Stored run that finished without a script error.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RunSummary {
    /// Parameters of the run, as JSON.
    pub params: serde_json::Value,
    /// Total profit in pips.
    pub total_pips: f64,
    /// Number of closed trades.
    pub trade_count: i32,
    /// Segment and benchmark metrics of the run, as JSON.
    pub metrics: serde_json::Value,
}

/// Returns up to `limit` runs of a completed batch that finished without a script error, the largest total profit first.
///
/// # Errors
///
/// - Returns an error if the query fails.
pub async fn top_runs(
    pool: &PgPool,
    batch_id: Uuid,
    limit: i64,
) -> Result<Vec<RunSummary>, SimError> {
    let rows = sqlx::query_as::<_, RunSummary>(
        r#"
        SELECT r.params AS params, r.total_pips AS total_pips,
               r.trade_count AS trade_count, r.metrics AS metrics
        FROM sim_runs r
        JOIN sim_batches b ON b.id = r.batch_id
        WHERE r.batch_id = $1 AND b.status = 'completed' AND r.status = 'ok'
        ORDER BY r.total_pips DESC, r.params::text COLLATE "C" ASC
        LIMIT $2
        "#,
    )
    .bind(batch_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
