use anyhow::Context as _;
use auto_trader_core::knowledge::KnowledgeStore;
use sqlx::PgPool;
use std::sync::Arc;

// ── Data types ────────────────────────────────────────────────────────────────

/// Per-strategy slice of weekly trade stats.
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

/// Parsed response from Gemini's parameter-proposal call.
#[derive(Debug, serde::Deserialize)]
struct GeminiProposal {
    params: serde_json::Value,
    rationale: String,
    #[serde(default)]
    expected_effect: String,
}

/// Minimum closed-trade count (in the past 7 days, for a given strategy) below
/// which the evolution loop refuses to propose updates. Wilson-Score lower
/// bound is meaningless at N=1〜2, and Gemini has historically over-fit to
/// 100%-win runs at that scale.
const MIN_TRADES_FOR_EVOLUTION: i64 = 5;

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the weekly evolution batch. Called from the daily batch when
/// day-of-week == Sunday (JST).
///
/// Workflow:
/// 1. Enumerate evolvable strategies (= rows in `strategy_params`).
/// 2. For each strategy: fetch stats, skip if low-sample, compute Wilson,
///    optionally pull KnowledgeStore context, ask Gemini for a proposal,
///    validate (strategy-specific or permissive default), persist, notify.
/// 3. Trigger a single KnowledgeStore merge after all strategies are processed.
pub async fn run(
    pool: &PgPool,
    knowledge: Option<&Arc<dyn KnowledgeStore>>,
    gemini_api_url: &str,
    gemini_api_key: &str,
    gemini_model: &str,
) -> anyhow::Result<()> {
    let strategies = list_evolvable_strategies(pool)
        .await
        .context("list_evolvable_strategies")?;
    if strategies.is_empty() {
        tracing::info!(
            "weekly_batch: strategy_params is empty; nothing to evolve. \
             INSERT a row for each strategy you want to be auto-tuned."
        );
        return Ok(());
    }
    tracing::info!(
        "weekly_batch: evolving {} strategies: {:?}",
        strategies.len(),
        strategies
    );

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
            // 1 strategy のエラーで全体を止めない
            tracing::error!("weekly_batch: strategy {strategy_name} failed: {e:#}");
        }
    }

    // KnowledgeStore merge は 1 回だけ。全 strategy 投入後に走らせる。
    if let Some(store) = knowledge {
        if let Err(err) = store.run_merge().await {
            tracing::warn!("weekly_batch: knowledge_store merge failed: {err:#}");
        } else {
            tracing::info!("weekly_batch: knowledge_store merge triggered");
        }
    }

    tracing::info!("weekly_batch: evolution run complete");
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
    tracing::info!("weekly_batch: starting evolution for {strategy_name}");

    // 1. Past-week stats (this strategy only)
    let stats = fetch_strategy_stats(pool, strategy_name)
        .await
        .context("fetch_strategy_stats")?;
    tracing::info!(
        "weekly_batch: {} had {} trades in the past 7 days",
        strategy_name,
        stats.total_trades
    );

    // 1b. Small-sample guard
    if stats.total_trades < MIN_TRADES_FOR_EVOLUTION {
        let msg = format!(
            "週次進化バッチ: {strategy_name} はサンプル不足 (n={}, 必要={MIN_TRADES_FOR_EVOLUTION}) のため、\
             現状パラメータを維持します。",
            stats.total_trades
        );
        tracing::info!("{msg}");
        insert_system_notification(pool, &msg)
            .await
            .context("insert_system_notification (small-sample)")?;
        return Ok(());
    }

    // 2. Wilson Score by regime (this strategy only)
    let wilson = compute_regime_wilson(pool, strategy_name)
        .await
        .context("compute_regime_wilson")?;

    // 3. Optional KnowledgeStore context
    let knowledge_context = fetch_knowledge_context(knowledge, strategy_name).await;

    // 4. Current params
    let current_params = load_current_params(pool, strategy_name)
        .await
        .context("load_current_params")?;

    // 5. Ask Gemini
    let prompt = build_gemini_prompt(
        strategy_name,
        &stats,
        knowledge_context.as_deref(),
        &current_params,
        &wilson,
    );
    // 5b. Gemini call — fail-fast notification if unavailable, so the operator
    //     sees something happened. Without this branch the run would silently
    //     fall through to validate_params(current_params), which for the
    //     permissive default validator (empty current_params object) bails
    //     out with no system_notification.
    let proposal = match call_gemini(gemini_api_url, gemini_api_key, gemini_model, &prompt).await {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!("weekly_batch: {strategy_name} Gemini call failed: {err:#}");
            let msg = format!(
                "週次進化バッチ: {strategy_name} は LLM 呼び出し失敗のため現状パラメータを維持します。理由: {err}"
            );
            insert_system_notification(pool, &msg)
                .await
                .context("insert_system_notification (gemini failure)")?;
            return Ok(());
        }
    };

    tracing::info!(
        "weekly_batch: {strategy_name} proposal rationale = {:?}",
        proposal.rationale
    );

    // 6. Validate + persist. `current_params` is passed so the permissive
    //    validator can enforce key-set/type parity with the existing row.
    let normalized = match validate_params(strategy_name, &proposal.params, &current_params) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("weekly_batch: {strategy_name} validation failed: {e}");
            tracing::warn!("weekly_batch: rejected params: {}", proposal.params);
            let msg = format!(
                "週次進化バッチ: {strategy_name} は LLM 提案の検証に失敗したため現状パラメータを維持します。理由: {e}"
            );
            insert_system_notification(pool, &msg)
                .await
                .context("insert_system_notification (validation failure)")?;
            return Ok(());
        }
    };
    persist_params(pool, strategy_name, &normalized)
        .await
        .context("persist_params")?;

    let notification_message = format!(
        "週次進化バッチ完了: {strategy_name}\n\
         根拠: {}\n\
         期待効果: {}\n\
         新パラメータ: {}",
        proposal.rationale, proposal.expected_effect, normalized,
    );
    insert_system_notification(pool, &notification_message)
        .await
        .context("insert_system_notification")?;

    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Strategies that are wired into the evolution loop. The convention is:
/// "to enable auto-tuning for strategy X, INSERT a row into strategy_params"
/// (which `donchian_trend_evolve_v1` does via migration; future strategies
/// can be added with an explicit INSERT or migration).
async fn list_evolvable_strategies(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT strategy_name FROM strategy_params ORDER BY strategy_name")
            .fetch_all(pool)
            .await
            .context("SELECT FROM strategy_params")?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

/// Query trade stats for the past 7 days for a single strategy.
///
/// Counts only `status = 'closed'` rows with a non-null `pnl_amount`, since
/// `closing` (in-flight) trades can carry partial fields that would distort
/// the win-rate computation. `COALESCE(SUM, 0)` is required because Postgres
/// aggregates over an empty set return NULL, and the tuple decoder expects
/// `i64` (not `Option<i64>`).
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
    .with_context(|| format!("SELECT weekly trade stats for {strategy_name}"))?;

    let (total_trades, wins, avg_pnl) = row.unwrap_or((0, 0, None));
    Ok(StrategyStats {
        total_trades,
        wins,
        avg_pnl: avg_pnl.unwrap_or(0.0),
    })
}

/// Compute Wilson Score 95% lower bounds per market regime for the past 7 days,
/// scoped to a single strategy. Only `status = 'closed'` rows are counted to
/// stay consistent with `fetch_strategy_stats`.
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
    .with_context(|| format!("SELECT regime Wilson stats for {strategy_name}"))?;

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

/// Dispatch validation to a per-strategy validator.
/// Unknown strategies use a permissive default that requires the proposed
/// JSON to match the existing `current_params` in both key set and per-key
/// type. This keeps "INSERT a row to enable evolution" as a one-step
/// ergonomic while still rejecting LLM hallucinations (new keys, missing
/// keys, type swaps). Strategies with non-trivial invariants should still
/// get a dedicated strict validator (cf. `validate_donchian_trend_evolve_v1`).
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

/// Strict validator for donchian_trend_evolve_v1. Returns a normalized JSON
/// containing only the three allowed keys to defeat hallucinated extras.
fn validate_donchian_trend_evolve_v1(
    params: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    // 必須キーの存在チェックを先に。デフォルト値で穴埋めしてから range を
    // 見ると「キー欠落」と「範囲外」が同じエラー扱いになって誤魔化される。
    let entry = params["entry_channel"]
        .as_i64()
        .context("entry_channel missing or non-integer")?;
    let exit = params["exit_channel"]
        .as_i64()
        .context("exit_channel missing or non-integer")?;
    let baseline = params["atr_baseline_bars"]
        .as_i64()
        .context("atr_baseline_bars missing or non-integer")?;

    if !(10..=30).contains(&entry) {
        anyhow::bail!("entry_channel {entry} out of range [10, 30]");
    }
    if !(5..=15).contains(&exit) {
        anyhow::bail!("exit_channel {exit} out of range [5, 15]");
    }
    if !(20..=100).contains(&baseline) {
        anyhow::bail!("atr_baseline_bars {baseline} out of range [20, 100]");
    }
    if exit >= entry {
        anyhow::bail!("exit_channel ({exit}) must be < entry_channel ({entry})");
    }

    Ok(serde_json::json!({
        "entry_channel": entry,
        "exit_channel": exit,
        "atr_baseline_bars": baseline,
    }))
}

/// Permissive fallback validator. Requires the proposed JSON to:
/// - be a non-empty object with no null values
/// - have an identical key set to `current` (no added or dropped keys)
/// - preserve the JSON type per key (e.g. number stays number)
///
/// Used for strategies with no dedicated validator registered yet. If
/// `current` is empty (first-time evolution), only the basic non-empty /
/// non-null check is applied — but operators should pre-seed
/// `strategy_params` with a known-good baseline rather than letting the
/// LLM define the initial schema.
fn validate_permissive(
    strategy_name: &str,
    proposed: &serde_json::Value,
    current: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let proposed_obj = proposed
        .as_object()
        .with_context(|| format!("{strategy_name}: params must be a JSON object"))?;
    if proposed_obj.is_empty() {
        anyhow::bail!("{strategy_name}: params object is empty (Gemini proposed no fields)");
    }
    if proposed_obj.values().any(|v| v.is_null()) {
        anyhow::bail!("{strategy_name}: params contain null values");
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
                "{strategy_name}: param key set differs from current. added={added:?} removed={removed:?}"
            );
        }
        for (key, current_val) in current_obj {
            let proposed_val = &proposed_obj[key];
            if !json_types_compatible(current_val, proposed_val) {
                anyhow::bail!(
                    "{strategy_name}: type mismatch for key '{key}': current={current_val} proposed={proposed_val}"
                );
            }
        }
    }

    tracing::warn!(
        "weekly_batch: {strategy_name} uses permissive validator (no strict schema). \
         Add a dedicated validator before relying on these params in production."
    );
    Ok(proposed.clone())
}

/// JSON-level compatibility check used by `validate_permissive`. Goes one
/// level finer than `discriminant(Value)` by distinguishing integer numbers
/// from floats — a strategy that uses `entry_channel: 20` (i64) should NOT
/// silently accept `entry_channel: 20.5` (f64), since downstream code may
/// `as_i64().unwrap()` on it.
fn json_types_compatible(current: &serde_json::Value, proposed: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (current, proposed) {
        (Value::Number(c), Value::Number(p)) => {
            // Treat i64 and u64 as the same "integer" subtype; f64 is distinct.
            let current_is_int = c.is_i64() || c.is_u64();
            let proposed_is_int = p.is_i64() || p.is_u64();
            current_is_int == proposed_is_int
        }
        _ => std::mem::discriminant(current) == std::mem::discriminant(proposed),
    }
}

/// Load the current JSON params blob for a strategy from `strategy_params`.
/// Returns an empty object `{}` when no row exists yet.
async fn load_current_params(pool: &PgPool, strategy: &str) -> anyhow::Result<serde_json::Value> {
    let row: Option<sqlx::types::Json<serde_json::Value>> =
        sqlx::query_scalar("SELECT params FROM strategy_params WHERE strategy_name = $1")
            .bind(strategy)
            .fetch_optional(pool)
            .await
            .with_context(|| format!("SELECT strategy_params for {strategy}"))?;

    Ok(row.map(|j| j.0).unwrap_or_else(|| serde_json::json!({})))
}

/// Persist updated params back to `strategy_params`.
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
    .with_context(|| format!("UPSERT strategy_params for {strategy}"))?;
    Ok(())
}

/// Insert a row into `system_notifications` (added in migration 20260410000001).
async fn insert_system_notification(pool: &PgPool, message: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO system_notifications (message) VALUES ($1)")
        .bind(message)
        .execute(pool)
        .await
        .context("INSERT system_notifications")?;
    Ok(())
}

/// Attempt to retrieve recent trade context from the knowledge store.
/// Returns `None` on failure (non-fatal — the batch continues without it).
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
            tracing::warn!("weekly_batch: knowledge search failed: {err:#}");
            None
        }
    }
}

/// Build the Gemini prompt from gathered stats, Wilson analysis, optional
/// KnowledgeStore context, and the current parameter blob.
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

    // Strategy stats section
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

    // Wilson Score section
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

    // KnowledgeStore context section
    if let Some(context) = knowledge_context {
        prompt.push_str("\n## 過去トレード学習コンテキスト\n");
        prompt.push_str(context);
        prompt.push('\n');
    }

    // Current params section
    prompt.push_str("\n## 現在のパラメータ\n");
    prompt.push_str(&current_params.to_string());
    prompt.push('\n');

    // Instructions — strategy-aware
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

/// Call the Gemini API and parse the `GeminiProposal` from the response text.
///
/// Returns `Err` when:
/// - `api_key` is empty (Gemini disabled in config)
/// - The HTTP call fails or returns non-2xx
/// - The response cannot be parsed as `GeminiProposal`
///
/// Callers are responsible for handling the error (the weekly batch surfaces
/// it as a `system_notification` and leaves params unchanged — no silent
/// fallback to current params, since that would mask LLM outages).
async fn call_gemini(
    api_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
) -> anyhow::Result<GeminiProposal> {
    if api_key.is_empty() {
        anyhow::bail!("Gemini API key is not configured");
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
        .context("POST to Gemini API")?;

    let status = response.status();
    let response_text = response
        .text()
        .await
        .context("read Gemini API response body")?;

    if !status.is_success() {
        anyhow::bail!("Gemini API returned {status}: {response_text}");
    }

    let raw: serde_json::Value =
        serde_json::from_str(&response_text).context("parse Gemini response JSON")?;

    let generated_text = raw
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|v| v.as_str())
        .with_context(|| format!("extract text from Gemini response; raw={response_text:.200}"))?;

    // The model is instructed to return JSON only; try parsing directly.
    // If the model wraps it in a code fence, strip that too.
    let json_text = extract_json(generated_text);

    serde_json::from_str::<GeminiProposal>(json_text)
        .with_context(|| format!("parse GeminiProposal JSON from: {json_text:.200}"))
}

/// Strip optional markdown code fences from a Gemini response so the
/// JSON parser has a clean input.
fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    // Handle ```json ... ``` or ``` ... ``` wrappers
    if let Some(inner) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
    {
        inner.trim_start().trim_end_matches("```").trim()
    } else {
        trimmed
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_plain_passes_through() {
        let input = r#"{"params":{},"rationale":"test","expected_effect":""}"#;
        assert_eq!(extract_json(input), input);
    }

    #[test]
    fn extract_json_strips_code_fence() {
        let input = "```json\n{\"params\":{},\"rationale\":\"r\",\"expected_effect\":\"\"}\n```";
        let extracted = extract_json(input);
        assert!(extracted.starts_with('{'));
        assert!(extracted.ends_with('}'));
    }

    #[test]
    fn extract_json_strips_plain_fence() {
        let input = "```\n{\"params\":{},\"rationale\":\"r\",\"expected_effect\":\"\"}\n```";
        let extracted = extract_json(input);
        assert!(extracted.starts_with('{'));
    }

    #[test]
    fn build_gemini_prompt_contains_key_sections() {
        let stats = StrategyStats {
            total_trades: 42,
            wins: 28,
            avg_pnl: 150.5,
        };
        let wilson = vec![RegimeAnalysis {
            regime: "trending".to_string(),
            trades: 20,
            wins: 15,
            wilson_lb: 0.55,
        }];
        let params = serde_json::json!({"entry_channel": 20});

        let prompt = build_gemini_prompt(
            "donchian_trend_evolve_v1",
            &stats,
            Some("vp context text"),
            &params,
            &wilson,
        );

        assert!(prompt.contains("42"));
        assert!(prompt.contains("donchian_trend_evolve_v1"));
        assert!(prompt.contains("trending"));
        assert!(prompt.contains("Wilson"));
        assert!(prompt.contains("vp context text"));
        assert!(prompt.contains("entry_channel"));
    }

    #[test]
    fn build_gemini_prompt_no_knowledge_context() {
        let stats = StrategyStats {
            total_trades: 0,
            wins: 0,
            avg_pnl: 0.0,
        };
        let wilson: Vec<RegimeAnalysis> = vec![];
        let params = serde_json::json!({});

        let prompt =
            build_gemini_prompt("donchian_trend_evolve_v1", &stats, None, &params, &wilson);

        // Without context the knowledge-store section should be absent
        assert!(!prompt.contains("過去トレード学習コンテキスト"));
        // But prompt must still include output format instructions
        assert!(prompt.contains("rationale"));
    }

    #[test]
    fn build_gemini_prompt_uses_strategy_specific_instructions() {
        let stats = StrategyStats {
            total_trades: 27,
            wins: 10,
            avg_pnl: -140.7,
        };
        let prompt = build_gemini_prompt(
            "bb_mean_revert_v1",
            &stats,
            None,
            &serde_json::json!({"something": 1}),
            &[],
        );

        assert!(prompt.contains("bb_mean_revert_v1"));
        // Unknown strategy → generic instructions, NOT donchian's specific keys
        assert!(!prompt.contains("entry_channel"));
        assert!(prompt.contains("現在のパラメータ構造を維持"));
    }

    #[test]
    fn validate_params_donchian_normalizes_and_drops_extras() {
        let proposed = serde_json::json!({
            "entry_channel": 20,
            "exit_channel": 10,
            "atr_baseline_bars": 50,
            "sl_pct": 0.03, // hallucinated extra
        });
        let current = serde_json::json!({});
        let v = validate_params("donchian_trend_evolve_v1", &proposed, &current).unwrap();
        assert_eq!(v["entry_channel"], 20);
        assert_eq!(v["exit_channel"], 10);
        assert_eq!(v["atr_baseline_bars"], 50);
        assert!(v.get("sl_pct").is_none(), "extra keys should be dropped");
    }

    #[test]
    fn validate_params_donchian_rejects_out_of_range() {
        let proposed = serde_json::json!({
            "entry_channel": 100, // out of [10, 30]
            "exit_channel": 10,
            "atr_baseline_bars": 50,
        });
        let current = serde_json::json!({});
        assert!(validate_params("donchian_trend_evolve_v1", &proposed, &current).is_err());
    }

    #[test]
    fn validate_params_donchian_rejects_exit_ge_entry() {
        let proposed = serde_json::json!({
            "entry_channel": 12,
            "exit_channel": 15, // exit >= entry
            "atr_baseline_bars": 50,
        });
        let current = serde_json::json!({});
        assert!(validate_params("donchian_trend_evolve_v1", &proposed, &current).is_err());
    }

    #[test]
    fn validate_params_permissive_accepts_matching_key_set() {
        let proposed = serde_json::json!({ "window": 20, "threshold": 1.5 });
        let current = serde_json::json!({ "window": 15, "threshold": 1.0 });
        let v = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap();
        assert_eq!(v["window"], 20);
    }

    #[test]
    fn validate_params_permissive_rejects_empty_object() {
        let proposed = serde_json::json!({});
        let current = serde_json::json!({ "window": 15 });
        assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_err());
    }

    #[test]
    fn validate_params_permissive_rejects_non_object() {
        let proposed = serde_json::json!([1, 2, 3]);
        let current = serde_json::json!({ "window": 15 });
        assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_err());
    }

    #[test]
    fn validate_params_permissive_rejects_null_values() {
        let proposed = serde_json::json!({"window": null});
        let current = serde_json::json!({ "window": 15 });
        assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_err());
    }

    #[test]
    fn validate_params_permissive_rejects_added_key() {
        // Hallucinated extra key — must be rejected when current has fixed schema.
        let proposed = serde_json::json!({ "window": 20, "threshold": 1.5, "rogue": 99 });
        let current = serde_json::json!({ "window": 15, "threshold": 1.0 });
        let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
        assert!(
            format!("{err}").contains("rogue"),
            "error should name the extra key: {err}"
        );
    }

    #[test]
    fn validate_params_permissive_rejects_dropped_key() {
        let proposed = serde_json::json!({ "window": 20 });
        let current = serde_json::json!({ "window": 15, "threshold": 1.0 });
        let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
        assert!(
            format!("{err}").contains("threshold"),
            "error should name the dropped key: {err}"
        );
    }

    #[test]
    fn validate_params_permissive_rejects_type_swap() {
        // LLM returned a string instead of a number — must be rejected.
        let proposed = serde_json::json!({ "window": "20" });
        let current = serde_json::json!({ "window": 15 });
        let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
        assert!(
            format!("{err}").contains("type mismatch"),
            "error should flag type mismatch: {err}"
        );
    }

    #[test]
    fn validate_params_permissive_rejects_int_to_float_swap() {
        // Codex round 2: discriminant(Value::Number) doesn't distinguish
        // integer from float. We do the extra check; this test pins it.
        let proposed = serde_json::json!({ "window": 20.5 });
        let current = serde_json::json!({ "window": 20 });
        let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
        assert!(
            format!("{err}").contains("type mismatch"),
            "int→float swap should be rejected: {err}"
        );
    }

    #[test]
    fn validate_params_permissive_accepts_float_to_float() {
        let proposed = serde_json::json!({ "threshold": 1.6 });
        let current = serde_json::json!({ "threshold": 1.2 });
        assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_ok());
    }

    #[test]
    fn validate_params_permissive_relaxes_when_current_empty() {
        // First-time evolution: no current schema to compare against; still
        // require non-empty + non-null but accept arbitrary keys.
        let proposed = serde_json::json!({ "anything": 1 });
        let current = serde_json::json!({});
        assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_ok());
    }
}
