use std::sync::Arc;

use anyhow::Context as _;
use auto_trader_core::knowledge::KnowledgeStore;
use sqlx::PgPool;

mod gemini;
mod params_validation;
mod proposals;

struct StrategyStats {
    total_trades: i64,
    wins: i64,
    avg_pnl: f64,
}

struct RegimeAnalysis {
    regime: String,
    trades: i64,
    wins: i64,
    wilson_lb: f64,
}

struct EvolutionEvidence {
    wilson: Vec<RegimeAnalysis>,
    knowledge_context: Option<String>,
    current_params: serde_json::Value,
}

const MIN_TRADES_FOR_EVOLUTION: i64 = 5;

/// Evolves the parameters of each strategy that has a row in `strategy_params`, then triggers a merge of `knowledge` if it is given, and returns once all strategies are processed. Does nothing if there is no row. For each strategy, the closed trades of the past 7 days are counted:
/// - with fewer than 5 trades, the parameters are kept and a system notification says so;
/// - otherwise Gemini at `gemini_api_url` is asked for a proposal using the statistics of those trades, the current parameters, the win rate lower bound of each regime and, if available, the strategy outcomes from `knowledge`; the proposal is recorded in `strategy_param_proposals` (validated or rejected) and announced in a system notification as not applied;
/// - if the Gemini call fails the parameters are kept and a system notification gives the reason; if the proposal does not validate, the rejection is recorded and a system notification gives the reason.
///
/// The weekly batch never writes to `strategy_params`: proposals are only recorded and never applied.
///
/// A failure for one strategy is logged as an error and does not stop the others, and a failure of the knowledge merge is logged as a warning.
///
/// # Errors
///
/// - Returns an error if the strategy names cannot be read from `strategy_params`.
pub async fn run(
    pool: &PgPool,
    knowledge: Option<&Arc<dyn KnowledgeStore>>,
    gemini_api_url: &str,
    gemini_api_key: &str,
    gemini_model: &str,
) -> anyhow::Result<()> {
    let strategies = list_evolvable_strategies(pool)
        .await
        .context("failed to list evolvable strategies")?;
    if strategies.is_empty() {
        tracing::info!(
            "strategy_params is empty, nothing to evolve; insert a row for each strategy to enable auto-tuning"
        );
        return Ok(());
    }
    tracing::info!(strategy_count = strategies.len(), strategies = ?strategies, "evolving strategies");

    evolve_each_strategy(
        pool,
        knowledge,
        gemini_api_url,
        gemini_api_key,
        gemini_model,
        &strategies,
    )
    .await;

    merge_knowledge_store(knowledge).await;

    tracing::info!("evolution run complete");
    Ok(())
}

async fn merge_knowledge_store(knowledge: Option<&Arc<dyn KnowledgeStore>>) {
    if let Some(store) = knowledge {
        if let Err(err) = store.run_merge().await {
            tracing::warn!(error = %format!("{err:#}"), "knowledge_store merge failed");
        } else {
            tracing::info!("knowledge_store merge triggered");
        }
    }
}

async fn evolve_each_strategy(
    pool: &PgPool,
    knowledge: Option<&Arc<dyn KnowledgeStore>>,
    gemini_api_url: &str,
    gemini_api_key: &str,
    gemini_model: &str,
    strategies: &[String],
) {
    for strategy_name in strategies {
        if let Err(e) = run_for_strategy(
            pool,
            knowledge,
            gemini_api_url,
            gemini_api_key,
            gemini_model,
            strategy_name,
        )
        .await
        {
            tracing::error!(
                strategy_name = %strategy_name,
                error = %format!("{e:#}"),
                "strategy evolution failed"
            );
        }
    }
}

async fn run_for_strategy(
    pool: &PgPool,
    knowledge: Option<&Arc<dyn KnowledgeStore>>,
    gemini_api_url: &str,
    gemini_api_key: &str,
    gemini_model: &str,
    strategy_name: &str,
) -> anyhow::Result<()> {
    tracing::info!(strategy_name = %strategy_name, "starting evolution");

    let stats = fetch_strategy_stats(pool, strategy_name)
        .await
        .context("failed to fetch strategy stats")?;
    tracing::info!(
        strategy_name = %strategy_name,
        trade_count = stats.total_trades,
        "trades in the past 7 days"
    );

    if stats.total_trades < MIN_TRADES_FOR_EVOLUTION {
        return keep_params_for_small_sample(pool, strategy_name, stats.total_trades).await;
    }

    let evidence = gather_evidence(pool, knowledge, strategy_name).await?;

    let prompt = gemini::build_gemini_prompt(
        strategy_name,
        &stats,
        evidence.knowledge_context.as_deref(),
        &evidence.current_params,
        &evidence.wilson,
    );

    let proposal = match request_proposal(
        pool,
        strategy_name,
        gemini_api_url,
        gemini_api_key,
        gemini_model,
        &prompt,
    )
    .await?
    {
        Some(proposal) => proposal,
        None => return Ok(()),
    };

    let validation = params_validation::validate_params(
        strategy_name,
        &proposal.params,
        &evidence.current_params,
    );
    let record = proposals::ProposalRecord {
        strategy_name,
        proposal: &proposal,
        evidence: proposals::evidence_json(&stats, &evidence),
        llm_model: gemini_model,
        base_params: &evidence.current_params,
    };
    match &validation {
        Ok(normalized) => proposals::record_proposal(pool, &record, normalized).await,
        Err(err) => proposals::keep_params_after_rejected_proposal(pool, &record, err).await,
    }
}

async fn gather_evidence(
    pool: &PgPool,
    knowledge: Option<&Arc<dyn KnowledgeStore>>,
    strategy_name: &str,
) -> anyhow::Result<EvolutionEvidence> {
    let wilson = compute_regime_wilson(pool, strategy_name)
        .await
        .context("failed to compute regime wilson scores")?;

    let knowledge_context = fetch_knowledge_context(knowledge, strategy_name).await;

    let current_params = load_current_params(pool, strategy_name)
        .await
        .context("failed to load current params")?;

    Ok(EvolutionEvidence {
        wilson,
        knowledge_context,
        current_params,
    })
}

async fn request_proposal(
    pool: &PgPool,
    strategy_name: &str,
    gemini_api_url: &str,
    gemini_api_key: &str,
    gemini_model: &str,
    prompt: &str,
) -> anyhow::Result<Option<gemini::GeminiProposal>> {
    let proposal =
        match gemini::call_gemini(gemini_api_url, gemini_api_key, gemini_model, prompt).await {
            Ok(proposal) => proposal,
            Err(err) => {
                keep_params_after_gemini_failure(pool, strategy_name, &err).await?;
                return Ok(None);
            }
        };

    tracing::info!(
        strategy_name = %strategy_name,
        rationale = ?proposal.rationale,
        "received gemini proposal"
    );

    Ok(Some(proposal))
}

async fn keep_params_for_small_sample(
    pool: &PgPool,
    strategy_name: &str,
    total_trades: i64,
) -> anyhow::Result<()> {
    let msg = format!(
        "週次進化バッチ: {strategy_name} はサンプル不足 (n={}, 必要={MIN_TRADES_FOR_EVOLUTION}) のため、\
         現状パラメータを維持します。",
        total_trades
    );
    tracing::info!(
        strategy_name = %strategy_name,
        trade_count = total_trades,
        min_trades = MIN_TRADES_FOR_EVOLUTION,
        "insufficient sample, keeping current params"
    );
    insert_system_notification(pool, &msg)
        .await
        .context("failed to insert system notification for small-sample skip")?;
    Ok(())
}

async fn keep_params_after_gemini_failure(
    pool: &PgPool,
    strategy_name: &str,
    err: &anyhow::Error,
) -> anyhow::Result<()> {
    tracing::warn!(
        strategy_name = %strategy_name,
        error = %format!("{err:#}"),
        "gemini call failed"
    );
    let msg = format!(
        "週次進化バッチ: {strategy_name} は LLM 呼び出し失敗のため現状パラメータを維持します。理由: {err}"
    );
    insert_system_notification(pool, &msg)
        .await
        .context("failed to insert system notification for gemini failure")?;
    Ok(())
}

async fn list_evolvable_strategies(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT strategy_name FROM strategy_params ORDER BY strategy_name")
            .fetch_all(pool)
            .await
            .context("failed to query strategy_params")?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

async fn fetch_strategy_stats(pool: &PgPool, strategy_name: &str) -> anyhow::Result<StrategyStats> {
    let row: Option<(i64, i64, Option<f64>)> = sqlx::query_as(
        r#"
        SELECT COUNT(*)::bigint                                                     AS trades,
               COALESCE(SUM(CASE WHEN pnl_amount > 0 THEN 1 ELSE 0 END), 0)::bigint AS wins,
               AVG(pnl_amount)::float8                                              AS avg_pnl
        FROM trades
        WHERE strategy_name = $1
          AND status = 'closed'
          AND pnl_amount IS NOT NULL
          AND exit_at > NOW() - INTERVAL '7 days'
        "#,
    )
    .bind(strategy_name)
    .fetch_optional(pool)
    .await
    .with_context(|| format!("failed to query weekly trade stats for strategy {strategy_name}"))?;

    let (total_trades, wins, avg_pnl) = row.unwrap_or((0, 0, None));
    Ok(StrategyStats {
        total_trades,
        wins,
        avg_pnl: avg_pnl.unwrap_or(0.0),
    })
}

async fn compute_regime_wilson(
    pool: &PgPool,
    strategy_name: &str,
) -> anyhow::Result<Vec<RegimeAnalysis>> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        r#"
        SELECT entry_indicators->>'regime'                                          AS regime,
               COUNT(*)::bigint                                                     AS trades,
               COALESCE(SUM(CASE WHEN pnl_amount > 0 THEN 1 ELSE 0 END), 0)::bigint AS wins
        FROM trades
        WHERE exit_at > NOW() - INTERVAL '7 days'
          AND status = 'closed'
          AND pnl_amount IS NOT NULL
          AND entry_indicators IS NOT NULL
          AND entry_indicators->>'regime' IS NOT NULL
          AND strategy_name = $1
        GROUP BY entry_indicators->>'regime'
        "#,
    )
    .bind(strategy_name)
    .fetch_all(pool)
    .await
    .with_context(|| format!("failed to query regime wilson stats for strategy {strategy_name}"))?;

    let analyses = rows
        .into_iter()
        .map(|(regime, trades, wins)| {
            let wilson_lb = crate::wilson::lower_bound_95(wins as u64, trades as u64);
            RegimeAnalysis {
                regime,
                trades,
                wins,
                wilson_lb,
            }
        })
        .collect();
    Ok(analyses)
}

async fn load_current_params(pool: &PgPool, strategy: &str) -> anyhow::Result<serde_json::Value> {
    let row: Option<sqlx::types::Json<serde_json::Value>> =
        sqlx::query_scalar("SELECT params FROM strategy_params WHERE strategy_name = $1")
            .bind(strategy)
            .fetch_optional(pool)
            .await
            .with_context(|| format!("failed to query strategy_params for strategy {strategy}"))?;

    Ok(row.map(|j| j.0).unwrap_or_else(|| serde_json::json!({})))
}

async fn insert_system_notification(pool: &PgPool, message: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO system_notifications (message) VALUES ($1)")
        .bind(message)
        .execute(pool)
        .await
        .context("failed to insert system_notifications row")?;
    Ok(())
}

async fn fetch_knowledge_context(
    knowledge: Option<&Arc<dyn KnowledgeStore>>,
    strategy_name: &str,
) -> Option<String> {
    let store = knowledge?;
    match store.search_strategy_outcomes(strategy_name, 5).await {
        Ok(res) => {
            let context = res
                .hits
                .into_iter()
                .map(|h| h.text)
                .collect::<Vec<_>>()
                .join("\n---\n");
            if context.is_empty() {
                None
            } else {
                Some(context)
            }
        }
        Err(err) => {
            tracing::warn!(error = %format!("{err:#}"), "knowledge search failed");
            None
        }
    }
}

#[cfg(test)]
mod tests;
