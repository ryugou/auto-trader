use std::sync::Arc;

use anyhow::Context as _;
use auto_trader_core::knowledge::KnowledgeStore;
use sqlx::PgPool;

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

#[derive(Debug, serde::Deserialize)]
struct GeminiProposal {
    params: serde_json::Value,
    rationale: String,
    #[serde(default)]
    expected_effect: String,
}

const MIN_TRADES_FOR_EVOLUTION: i64 = 5;

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

    for strategy_name in &strategies {
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

    if let Some(store) = knowledge {
        if let Err(err) = store.run_merge().await {
            tracing::warn!(error = %format!("{err:#}"), "knowledge_store merge failed");
        } else {
            tracing::info!("knowledge_store merge triggered");
        }
    }

    tracing::info!("evolution run complete");
    Ok(())
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
        let msg = format!(
            "週次進化バッチ: {strategy_name} はサンプル不足 (n={}, 必要={MIN_TRADES_FOR_EVOLUTION}) のため、\
             現状パラメータを維持します。",
            stats.total_trades
        );
        tracing::info!(
            strategy_name = %strategy_name,
            trade_count = stats.total_trades,
            min_trades = MIN_TRADES_FOR_EVOLUTION,
            "insufficient sample, keeping current params"
        );
        insert_system_notification(pool, &msg)
            .await
            .context("failed to insert system notification for small-sample skip")?;
        return Ok(());
    }

    let wilson = compute_regime_wilson(pool, strategy_name)
        .await
        .context("failed to compute regime wilson scores")?;

    let knowledge_context = fetch_knowledge_context(knowledge, strategy_name).await;

    let current_params = load_current_params(pool, strategy_name)
        .await
        .context("failed to load current params")?;

    let prompt = build_gemini_prompt(
        strategy_name,
        &stats,
        knowledge_context.as_deref(),
        &current_params,
        &wilson,
    );
    let proposal = match call_gemini(gemini_api_url, gemini_api_key, gemini_model, &prompt).await {
        Ok(p) => p,
        Err(err) => {
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
            return Ok(());
        }
    };

    tracing::info!(
        strategy_name = %strategy_name,
        rationale = ?proposal.rationale,
        "received gemini proposal"
    );

    let normalized = match validate_params(strategy_name, &proposal.params, &current_params) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(strategy_name = %strategy_name, error = %e, "validation failed");
            tracing::warn!(strategy_name = %strategy_name, params = %proposal.params, "rejected params");
            let msg = format!(
                "週次進化バッチ: {strategy_name} は LLM 提案の検証に失敗したため現状パラメータを維持します。理由: {e}"
            );
            insert_system_notification(pool, &msg)
                .await
                .context("failed to insert system notification for validation failure")?;
            return Ok(());
        }
    };
    persist_params(pool, strategy_name, &normalized)
        .await
        .context("failed to persist params")?;

    let notification_message = format!(
        "週次進化バッチ完了: {strategy_name}\n\
         根拠: {}\n\
         期待効果: {}\n\
         新パラメータ: {}",
        proposal.rationale, proposal.expected_effect, normalized,
    );
    insert_system_notification(pool, &notification_message)
        .await
        .context("failed to insert system notification")?;

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

fn validate_params(
    strategy_name: &str,
    proposed: &serde_json::Value,
    current: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    match strategy_name {
        "donchian_trend_evolve_v1" => validate_donchian_trend_evolve_v1(proposed),
        _ => validate_permissive(strategy_name, proposed, current),
    }
}

fn validate_donchian_trend_evolve_v1(
    params: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let entry = params["entry_channel"]
        .as_i64()
        .context("invalid entry_channel: missing or non-integer")?;
    let exit = params["exit_channel"]
        .as_i64()
        .context("invalid exit_channel: missing or non-integer")?;
    let baseline = params["atr_baseline_bars"]
        .as_i64()
        .context("invalid atr_baseline_bars: missing or non-integer")?;

    if !(10..=30).contains(&entry) {
        anyhow::bail!("invalid entry_channel: out of range [10, 30]");
    }
    if !(5..=15).contains(&exit) {
        anyhow::bail!("invalid exit_channel: out of range [5, 15]");
    }
    if !(20..=100).contains(&baseline) {
        anyhow::bail!("invalid atr_baseline_bars: out of range [20, 100]");
    }
    if exit >= entry {
        anyhow::bail!("invalid exit_channel: must be less than entry_channel");
    }

    Ok(serde_json::json!({
        "entry_channel": entry,
        "exit_channel": exit,
        "atr_baseline_bars": baseline,
    }))
}

fn validate_permissive(
    strategy_name: &str,
    proposed: &serde_json::Value,
    current: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let proposed_obj = proposed
        .as_object()
        .with_context(|| format!("invalid params \"{strategy_name}\": must be a json object"))?;
    if proposed_obj.is_empty() {
        anyhow::bail!("invalid params \"{strategy_name}\": empty object");
    }
    if proposed_obj.values().any(|v| v.is_null()) {
        anyhow::bail!("invalid params \"{strategy_name}\": contains null values");
    }

    if let Some(current_obj) = current.as_object()
        && !current_obj.is_empty()
    {
        use std::collections::BTreeSet;
        let current_keys: BTreeSet<&str> = current_obj.keys().map(String::as_str).collect();
        let proposed_keys: BTreeSet<&str> = proposed_obj.keys().map(String::as_str).collect();
        let added: Vec<&&str> = proposed_keys.difference(&current_keys).collect();
        let removed: Vec<&&str> = current_keys.difference(&proposed_keys).collect();
        if !added.is_empty() || !removed.is_empty() {
            anyhow::bail!(
                "invalid params \"{strategy_name}\": key set differs from current (added={added:?}, removed={removed:?})"
            );
        }
        for (key, current_val) in current_obj {
            let proposed_val = &proposed_obj[key];
            if !json_types_compatible(current_val, proposed_val) {
                anyhow::bail!(
                    "invalid params \"{strategy_name}\": type mismatch for key \"{key}\" (current={current_val}, proposed={proposed_val})"
                );
            }
        }
    }

    tracing::warn!(
        strategy_name = %strategy_name,
        "uses permissive validator (no strict schema); add a dedicated validator before relying on these params in production"
    );
    Ok(proposed.clone())
}

fn json_types_compatible(current: &serde_json::Value, proposed: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (current, proposed) {
        (Value::Number(c), Value::Number(p)) => {
            let current_is_int = c.is_i64() || c.is_u64();
            let proposed_is_int = p.is_i64() || p.is_u64();
            current_is_int == proposed_is_int
        }
        _ => std::mem::discriminant(current) == std::mem::discriminant(proposed),
    }
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

async fn persist_params(
    pool: &PgPool,
    strategy: &str,
    params: &serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO strategy_params (strategy_name, params, updated_at)
        VALUES ($1, $2, NOW())
        ON CONFLICT (strategy_name)
        DO UPDATE SET params = EXCLUDED.params, updated_at = NOW()
        "#,
    )
    .bind(strategy)
    .bind(sqlx::types::Json(params))
    .execute(pool)
    .await
    .with_context(|| format!("failed to upsert strategy_params for strategy {strategy}"))?;
    Ok(())
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

fn build_gemini_prompt(
    strategy_name: &str,
    stats: &StrategyStats,
    knowledge_context: Option<&str>,
    current_params: &serde_json::Value,
    wilson: &[RegimeAnalysis],
) -> String {
    let mut prompt = String::with_capacity(2048);

    prompt.push_str(
        "あなたは自動売買システムのパラメータ最適化エキスパートです。\
         以下のデータを分析し、戦略パラメータの更新提案をJSON形式のみで返してください。\
         JSON以外のテキストは一切含めないこと。\n\n",
    );

    let win_rate = if stats.total_trades > 0 {
        stats.wins as f64 / stats.total_trades as f64 * 100.0
    } else {
        0.0
    };
    prompt.push_str(&format!("## 対象戦略: {}\n", strategy_name));
    prompt.push_str("## 過去7日間のトレード統計\n");
    prompt.push_str(&format!(
        "総トレード数: {}, 勝率: {:.1}%, 平均損益: {:.4}\n",
        stats.total_trades, win_rate, stats.avg_pnl
    ));

    prompt.push_str("\n## レジーム別 Wilson Score 分析 (95%信頼区間下限)\n");
    if wilson.is_empty() {
        prompt.push_str("データなし\n");
    } else {
        for analysis in wilson {
            let regime_win_rate = if analysis.trades > 0 {
                analysis.wins as f64 / analysis.trades as f64 * 100.0
            } else {
                0.0
            };
            prompt.push_str(&format!(
                "  - {}: {}トレード, 勝率={:.1}%, Wilson下限={:.4}\n",
                analysis.regime, analysis.trades, regime_win_rate, analysis.wilson_lb
            ));
        }
    }

    if let Some(context) = knowledge_context {
        prompt.push_str("\n## 過去トレード学習コンテキスト\n");
        prompt.push_str(context);
        prompt.push('\n');
    }

    prompt.push_str("\n## 現在のパラメータ\n");
    prompt.push_str(&current_params.to_string());
    prompt.push('\n');

    prompt.push_str("\n## 指示\n");
    prompt.push_str(&format!(
        "上記データを踏まえ、`{strategy_name}` 戦略の最適なパラメータを提案してください。\n"
    ));
    match strategy_name {
        "donchian_trend_evolve_v1" => {
            prompt.push_str(
                "パラメータキー: entry_channel (整数), exit_channel (整数), atr_baseline_bars (整数)。\n\
                 制約: exit_channel < entry_channel、いずれも正の整数。\n",
            );
        }
        _ => {
            prompt.push_str(
                "現在のパラメータ構造を維持し、各キーの型と意味的妥当性を保ったまま値のみ調整してください。\n\
                 新しいキーの追加は禁止。\n",
            );
        }
    }
    prompt.push_str(
        "以下のJSON形式のみで応答すること:\n\
         {\"params\":{...},\"rationale\":\"変更理由\",\"expected_effect\":\"期待効果\"}\n",
    );

    prompt
}

async fn call_gemini(
    api_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
) -> anyhow::Result<GeminiProposal> {
    if api_key.is_empty() {
        anyhow::bail!("invalid api_key: not configured");
    }

    let url = format!("{api_url}/v1beta/models/{model}:generateContent");
    let body = serde_json::json!({
        "contents": [{"parts": [{"text": prompt}]}]
    });

    let http_client = reqwest::Client::new();
    let response = http_client
        .post(&url)
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
        .context("failed to post to gemini api")?;

    let status = response.status();
    let response_text = response
        .text()
        .await
        .context("failed to read gemini api response body")?;

    if !status.is_success() {
        anyhow::bail!(
            "failed to call gemini api: non-2xx response (status={status}, body={response_text})"
        );
    }

    let raw: serde_json::Value =
        serde_json::from_str(&response_text).context("failed to parse gemini response json")?;

    let generated_text = raw
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|v| v.as_str())
        .with_context(|| {
            format!("failed to extract text from gemini response (raw={response_text:.200})")
        })?;

    let json_text = extract_json(generated_text);

    serde_json::from_str::<GeminiProposal>(json_text)
        .with_context(|| format!("failed to parse GeminiProposal json (text={json_text:.200})"))
}

fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
    {
        inner.trim_start().trim_end_matches("```").trim()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests;
