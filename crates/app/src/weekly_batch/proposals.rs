use anyhow::Context as _;
use sqlx::PgPool;
use uuid::Uuid;

use super::{EvolutionEvidence, StrategyStats, gemini, insert_system_notification};

pub(super) struct ProposalRecord<'a> {
    pub(super) strategy_name: &'a str,
    pub(super) proposal: &'a gemini::GeminiProposal,
    pub(super) evidence: serde_json::Value,
    pub(super) llm_model: &'a str,
    pub(super) base_params: &'a serde_json::Value,
}

pub(super) fn evidence_json(
    stats: &StrategyStats,
    evidence: &EvolutionEvidence,
) -> serde_json::Value {
    let wilson: Vec<serde_json::Value> = evidence
        .wilson
        .iter()
        .map(|r| {
            serde_json::json!({
                "regime": r.regime,
                "trades": r.trades,
                "wins": r.wins,
                "wilson_lb": r.wilson_lb,
            })
        })
        .collect();
    serde_json::json!({
        "total_trades": stats.total_trades,
        "wins": stats.wins,
        "avg_pnl": stats.avg_pnl,
        "wilson": wilson,
        "has_knowledge_context": evidence.knowledge_context.is_some(),
    })
}

pub(super) async fn keep_params_after_rejected_proposal(
    pool: &PgPool,
    record: &ProposalRecord<'_>,
    err: &anyhow::Error,
) -> anyhow::Result<()> {
    let proposed = &record.proposal.params;
    let strategy_name = record.strategy_name;
    tracing::warn!(strategy_name = %strategy_name, error = %err, "validation failed");
    tracing::warn!(strategy_name = %strategy_name, params = %proposed, "rejected params");
    insert_proposal(pool, record, Err(err))
        .await
        .context("failed to record rejected proposal")?;
    let msg = format!(
        "週次進化バッチ: {strategy_name} は LLM 提案の検証に失敗したため現状パラメータを維持します。理由: {err} 提案パラメータ: {}",
        proposed
    );
    insert_system_notification(pool, &msg)
        .await
        .context("failed to insert system notification for validation failure")?;
    Ok(())
}

pub(super) async fn record_proposal(
    pool: &PgPool,
    record: &ProposalRecord<'_>,
    normalized: &serde_json::Value,
) -> anyhow::Result<()> {
    let id = insert_proposal(pool, record, Ok(normalized))
        .await
        .context("failed to record proposal")?;

    let notification_message = format!(
        "週次進化バッチ: {} の提案を記録しました(未反映、承認待ち)\n\
         根拠: {}\n\
         期待効果: {}\n\
         提案パラメータ: {}\n\
         提案ID: {id}",
        record.strategy_name,
        record.proposal.rationale,
        record.proposal.expected_effect,
        normalized,
    );
    insert_system_notification(pool, &notification_message)
        .await
        .context("failed to insert system notification")?;

    Ok(())
}

async fn insert_proposal(
    pool: &PgPool,
    record: &ProposalRecord<'_>,
    outcome: Result<&serde_json::Value, &anyhow::Error>,
) -> anyhow::Result<Uuid> {
    let (normalized, validation_error) = match outcome {
        Ok(normalized) => (Some(sqlx::types::Json(normalized)), None),
        Err(err) => (None, Some(err.to_string())),
    };
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO strategy_param_proposals
            (id, strategy_name, base_params, proposed_params, normalized_params,
             validation_error, rationale, expected_effect, evidence, llm_model)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        "#,
    )
    .bind(id)
    .bind(record.strategy_name)
    .bind(sqlx::types::Json(record.base_params))
    .bind(sqlx::types::Json(&record.proposal.params))
    .bind(normalized)
    .bind(validation_error)
    .bind(&record.proposal.rationale)
    .bind(&record.proposal.expected_effect)
    .bind(sqlx::types::Json(&record.evidence))
    .bind(record.llm_model)
    .execute(pool)
    .await
    .with_context(|| {
        format!(
            "failed to insert strategy_param_proposals row for strategy {}",
            record.strategy_name
        )
    })?;
    Ok(id)
}
