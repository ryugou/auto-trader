//! `sim_scripts`、`sim_batches`、`sim_runs` の読み書き（spec 12 章）。
//!
//! この基盤がデータを失っても良いケースは存在しない前提で書く: スクリプトの重複登録は
//! 既存行を変更せず idempotent にし（`register_script`）、バッチの状態遷移は
//! `running → completed`/`failed` の一方向のみとし（`finish_batch`）、個々のシミュレーション
//! 結果は完了ごとに 1 件保存する（`save_run`）。`sim_runs` の `UNIQUE (batch_id, params)`
//! 制約により、同じバッチへ同じパラメータを 2 回保存しようとすると DB 層でエラーになる
//! （呼び出し元の重複呼び出しを黙って上書きしない）。

use crate::engine::RunStatus;
use crate::error::SimError;
use crate::script::{CompiledScript, ParamSet, ParamValue};
use crate::sweep::RunRecord;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// `secs`（UTC エポック秒）を `TIMESTAMPTZ` 列に対応する `DateTime<Utc>` に変換する。
/// `create_batch` の `period_from`/`period_to` は `Dataset::new` の成功後に呼び出し元（Task 9
/// の CLI）が渡す値であり、常に妥当な UTC エポック秒である契約だが、DB 層の入口として
/// 契約違反を黙って通さず報告する（`data.rs::epoch_secs_to_datetime` と同じ方針）。
fn epoch_to_datetime(secs: i64, context: &str) -> Result<DateTime<Utc>, SimError> {
    Utc.timestamp_opt(secs, 0).single().ok_or_else(|| {
        SimError::Other(anyhow::anyhow!(
            "{context}: epoch seconds {secs} is not a representable UTC instant"
        ))
    })
}

/// `open_time`（UTC エポック秒）を RFC 3339 に変換する（spec 12 章の `error` 列の接頭辞）。
/// `RunRecord` のフィールドは pub であり、`RunStatus::ScriptError.open_time` には
/// 表現不能な秒（例: `i64::MAX`）も渡せるため、panic せず `SimError::Other` を返す。
fn rfc3339(open_time: i64) -> Result<String, SimError> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(open_time, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .ok_or_else(|| {
            SimError::Other(anyhow::anyhow!(
                "open_time {open_time} is not a representable UTC instant"
            ))
        })
}

/// `ParamSet` を `sim_runs.params`(JSONB)の形へ変換する。`ParamSet` は `BTreeMap` なので
/// キーは名前の辞書順で並ぶが、`serde_json::Map` は既定で内部表現も `BTreeMap` であり
/// （`preserve_order` feature 未使用）、出力される JSON のキー順は常に辞書順になる。
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

// ---------------------------------------------------------------------------
// sim_scripts
// ---------------------------------------------------------------------------

/// `script` を `sim_scripts` に登録する（spec 12 章）。同じ `source_sha256` の行が既に
/// 存在する場合、既存行は変更せず（`name` で上書きしない）、その `id` を返す。
pub async fn register_script(
    pool: &PgPool,
    name: &str,
    script: &CompiledScript,
    origin: &str,
    parent_id: Option<Uuid>,
) -> Result<Uuid, SimError> {
    // spec 12 章: origin = 'human' の場合、parent_id は NULL とする。
    if origin == "human" && parent_id.is_some() {
        return Err(SimError::Args(format!(
            "register_script: origin = 'human' must not have a parent_id \
             (origin={origin}, parent_id={parent_id:?})"
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
            // ON CONFLICT で行が挿入されなかった = 同じ source_sha256 の行が既に存在する。
            // その既存行の id を返す(既存行は変更しない: spec 12 章)。
            let existing_id: Uuid =
                sqlx::query_scalar("SELECT id FROM sim_scripts WHERE source_sha256 = $1")
                    .bind(&script.sha256)
                    .fetch_one(pool)
                    .await?;
            Ok(existing_id)
        }
    }
}

// ---------------------------------------------------------------------------
// sim_batches
// ---------------------------------------------------------------------------

/// `create_batch` への入力（spec 12 章）。
#[derive(Debug, Clone)]
pub struct NewBatch {
    pub script_id: Uuid,
    pub period_from: i64,
    pub period_to: i64,
    pub bar_count: i32,
    pub total_runs: i32,
    pub config: serde_json::Value,
}

/// `sim_batches.status`（spec 12 章）。
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

/// `sim_batches` の行を 1 つ作る（spec 12 章）。作成直後は `status = 'running'`、
/// `finished_at` は NULL。
pub async fn create_batch(pool: &PgPool, batch: &NewBatch) -> Result<Uuid, SimError> {
    let id = Uuid::new_v4();
    let period_from = epoch_to_datetime(batch.period_from, "create_batch period_from")?;
    let period_to = epoch_to_datetime(batch.period_to, "create_batch period_to")?;
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

/// `sim_batches` を `running` から `completed`/`failed` へ遷移させ、`finished_at` を設定する
/// （spec 12 章）。遷移は一方向のみ:
///
/// - `status = Running` を指定された場合は DB に触れず `SimError::Args` を返す。
/// - 対象の行が存在しない、または既に `running` でない（終了済みの）場合は
///   `SimError::Args` を返し、行は変更しない。
pub async fn finish_batch(
    pool: &PgPool,
    batch_id: Uuid,
    status: BatchStatus,
) -> Result<(), SimError> {
    if status == BatchStatus::Running {
        return Err(SimError::Args(format!(
            "finish_batch: batch {batch_id} cannot be transitioned to 'running' \
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
            "finish_batch: batch {batch_id} could not be set to '{}': \
             it does not exist or is not in 'running' state",
            status.as_sql()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// sim_runs
// ---------------------------------------------------------------------------

/// `record` を `sim_runs` に 1 行保存する（spec 12 章）。
///
/// - `status = Ok`: 指標の列と `metrics` を保存し、`error` を NULL にする。
/// - `status = ScriptError`: `error` に `<open_time（RFC 3339）> <メッセージ>` を保存し、
///   指標の列と `metrics` を NULL にする。
///
/// 同じ `batch_id` と `params` の行が既に存在する場合、`sim_runs` の
/// `UNIQUE (batch_id, params)` 制約により `SimError::Db` を返す（黙って上書きしない）。
pub async fn save_run(pool: &PgPool, batch_id: Uuid, record: &RunRecord) -> Result<Uuid, SimError> {
    let id = Uuid::new_v4();
    let params = params_json(&record.params);

    match (&record.status, &record.metrics) {
        (RunStatus::Ok, None) => {
            return Err(SimError::Other(anyhow::anyhow!(
                "save_run: inconsistent RunRecord (status = Ok but metrics = None): \
                 batch_id={batch_id}, params={params}"
            )));
        }
        (RunStatus::ScriptError { .. }, Some(_)) => {
            return Err(SimError::Other(anyhow::anyhow!(
                "save_run: inconsistent RunRecord (status = ScriptError but metrics = Some): \
                 batch_id={batch_id}, params={params}"
            )));
        }
        (RunStatus::Ok, Some(metrics)) => {
            let trade_count = i32::try_from(metrics.trade_count).map_err(|e| {
                SimError::Other(anyhow::anyhow!(
                    "save_run: trade_count {} does not fit sim_runs.trade_count (i32): {e}: \
                     batch_id={batch_id}, params={params}",
                    metrics.trade_count
                ))
            })?;
            let protective_stop_count =
                i32::try_from(metrics.protective_stop_count).map_err(|e| {
                    SimError::Other(anyhow::anyhow!(
                        "save_run: protective_stop_count {} does not fit \
                         sim_runs.protective_stop_count (i32): {e}: \
                         batch_id={batch_id}, params={params}",
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
                    "save_run: {e}: batch_id={batch_id}, params={params}"
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

/// `top_runs` が返す 1 行（spec 13 章: `sweep` の上位件数の出力に使う）。
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RunSummary {
    pub params: serde_json::Value,
    pub total_pips: f64,
    pub trade_count: i32,
    pub metrics: serde_json::Value,
}

/// `completed` のバッチについて、`total_pips` 降順（同値は `params` の JSON 文字列の昇順）で
/// 上位 `limit` 件を返す（spec 13 章）。同値の比較は DB のロケール照合順序に依存しないよう
/// `COLLATE "C"`（バイト順）で行う。`running`・`failed` のバッチは空を返す（spec 12 章:
/// `completed` 以外のバッチは結果の出力と後続の評価の対象にしない）。
///
/// `r.status = 'ok'` の行だけを対象にする: `script_error` の行は指標の列が NULL であり、
/// 比較・出力の対象にならない。
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
