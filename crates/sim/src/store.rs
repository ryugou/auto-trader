use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::data::{epoch_secs_to_datetime, format_epoch_secs_rfc3339};
use crate::engine::RunStatus;
use crate::error::SimError;
use crate::script::{CompiledScript, ParamSet, ParamValue};
use crate::sweep::RunRecord;

fn rfc3339(open_time: i64) -> Result<String, SimError> {
    format_epoch_secs_rfc3339(open_time).ok_or_else(|| {
        SimError::Other(anyhow::anyhow!(
            "invalid open_time: {open_time} is not a representable UTC instant"
        ))
    })
}

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

#[derive(Debug, Clone)]
pub struct NewBatch {
    pub script_id: Uuid,
    pub period_from: i64,
    pub period_to: i64,
    pub bar_count: i32,
    pub total_runs: i32,
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchStatus {
    Running,
    Completed,
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
            let trade_count = i32::try_from(metrics.trade_count).map_err(|e| {
                SimError::Other(anyhow::anyhow!(
                    "invalid run record: trade_count {} does not fit sim_runs.trade_count as \
                     i32: {e} (batch_id={batch_id}, params={params})",
                    metrics.trade_count
                ))
            })?;
            let protective_stop_count =
                i32::try_from(metrics.protective_stop_count).map_err(|e| {
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
        }
        (RunStatus::ScriptError { open_time, message }, None) => {
            let open_time = rfc3339(*open_time).map_err(|e| {
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
        }
    }

    Ok(id)
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RunSummary {
    pub params: serde_json::Value,
    pub total_pips: f64,
    pub trade_count: i32,
    pub metrics: serde_json::Value,
}

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
