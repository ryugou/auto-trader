use anyhow::Context as _;
use auto_trader_vegapunk::client::VegapunkClient;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Mutex;

// ── Data types ────────────────────────────────────────────────────────────────

struct WeeklyStats {
    total_trades: i64,
    /// Each entry is `(strategy_name, trades, wins, avg_pnl)`.
    by_strategy: Vec<(String, i64, i64, f64)>,
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

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the weekly evolution batch. Called from the daily batch when
/// day-of-week == Sunday (JST).
///
/// Workflow:
/// 1. Fetch the past-7-day trade stats from the DB.
/// 2. Compute Wilson-Score lower bounds per regime.
/// 3. Optionally query Vegapunk for recent trade context.
/// 4. Load the current evolve-strategy params from `strategy_params`.
/// 5. Ask Gemini to propose updated params.
/// 6. Persist the proposed params and emit a `system_notifications` row.
/// 7. Trigger a Vegapunk merge so the ingested context is consolidated.
pub async fn run(
    pool: &PgPool,
    vegapunk: Option<&Arc<Mutex<VegapunkClient>>>,
    gemini_api_url: &str,
    gemini_api_key: &str,
    gemini_model: &str,
) -> anyhow::Result<()> {
    const STRATEGY: &str = "donchian_trend_evolve_v1";

    tracing::info!("weekly_batch: starting evolution run for {STRATEGY}");

    // 1. Past-week stats
    let stats = fetch_weekly_stats(pool)
        .await
        .context("fetch_weekly_stats")?;
    tracing::info!(
        "weekly_batch: {} trades in the past 7 days",
        stats.total_trades
    );

    // 2. Wilson Score by regime
    let wilson = compute_regime_wilson(pool)
        .await
        .context("compute_regime_wilson")?;

    // 3. Optional Vegapunk context
    let vp_context = fetch_vegapunk_context(vegapunk, &stats).await;

    // 4. Current params from DB
    let current_params = load_current_params(pool, STRATEGY)
        .await
        .context("load_current_params")?;

    // 5. Ask Gemini for a proposal
    let prompt = build_gemini_prompt(&stats, vp_context.as_deref(), &current_params, &wilson);
    let proposal = call_gemini(gemini_api_url, gemini_api_key, gemini_model, &prompt)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!("weekly_batch: Gemini call failed ({err:#}); using fallback params");
            GeminiProposal {
                params: current_params.clone(),
                rationale: "LLM unavailable".to_string(),
                expected_effect: String::new(),
            }
        });

    tracing::info!(
        "weekly_batch: proposal rationale = {:?}",
        proposal.rationale
    );

    // 6. Validate and persist proposed params
    if let Err(e) = validate_params(&proposal.params) {
        tracing::warn!("weekly_batch: LLM proposed invalid params, rejecting: {e}");
        tracing::warn!("weekly_batch: rejected params: {}", proposal.params);
        return Ok(());
    }
    persist_params(pool, STRATEGY, &proposal.params)
        .await
        .context("persist_params")?;

    let notification_message = format!(
        "週次進化バッチ完了: {STRATEGY}\n\
         根拠: {}\n\
         期待効果: {}\n\
         新パラメータ: {}",
        proposal.rationale, proposal.expected_effect, proposal.params,
    );
    insert_system_notification(pool, &notification_message)
        .await
        .context("insert_system_notification")?;

    // 7. Vegapunk merge (best-effort)
    if let Some(vp) = vegapunk {
        let mut client = vp.lock().await;
        if let Err(err) = client.merge().await {
            tracing::warn!("weekly_batch: Vegapunk merge failed: {err:#}");
        } else {
            tracing::info!("weekly_batch: Vegapunk merge triggered");
        }
    }

    tracing::info!("weekly_batch: evolution run complete");
    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Query trade stats for the past 7 days, grouped by strategy.
async fn fetch_weekly_stats(pool: &PgPool) -> anyhow::Result<WeeklyStats> {
    // sqlx::FromRow on an anonymous struct would require a named type;
    // using query_as with a tuple is simpler here.
    let rows: Vec<(String, i64, i64, f64)> = sqlx::query_as(
        r#"
        SELECT strategy_name,
               COUNT(*)::bigint                                          AS trades,
               SUM(CASE WHEN pnl_amount > 0 THEN 1 ELSE 0 END)::bigint AS wins,
               AVG(pnl_amount)::float8                                  AS avg_pnl
        FROM trades
        WHERE exit_at > NOW() - INTERVAL '7 days'
        GROUP BY strategy_name
        "#,
    )
    .fetch_all(pool)
    .await
    .context("SELECT weekly trade stats")?;

    let total_trades = rows.iter().map(|(_, trades, _, _)| trades).sum();
    Ok(WeeklyStats {
        total_trades,
        by_strategy: rows,
    })
}

/// Compute Wilson Score 95% lower bounds per market regime for the past 7 days.
async fn compute_regime_wilson(pool: &PgPool) -> anyhow::Result<Vec<RegimeAnalysis>> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        r#"
        SELECT entry_indicators->>'regime'                              AS regime,
               COUNT(*)::bigint                                         AS trades,
               SUM(CASE WHEN pnl_amount > 0 THEN 1 ELSE 0 END)::bigint AS wins
        FROM trades
        WHERE exit_at > NOW() - INTERVAL '7 days'
          AND entry_indicators IS NOT NULL
          AND entry_indicators->>'regime' IS NOT NULL
          AND strategy_name LIKE 'donchian_trend%'
        GROUP BY entry_indicators->>'regime'
        "#,
    )
    .fetch_all(pool)
    .await
    .context("SELECT regime Wilson stats")?;

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

/// Validate that LLM-proposed params are within safe bounds.
/// Rejects any proposal with out-of-range values to prevent the
/// evolve strategy from running with dangerous parameters.
fn validate_params(params: &serde_json::Value) -> anyhow::Result<()> {
    let entry = params["entry_channel"].as_u64().unwrap_or(20);
    let exit = params["exit_channel"].as_u64().unwrap_or(10);
    let sl = params["sl_pct"].as_f64().unwrap_or(0.03);
    let alloc = params["allocation_pct"].as_f64().unwrap_or(1.0);
    let baseline = params["atr_baseline_bars"].as_u64().unwrap_or(50);

    if !(10..=30).contains(&entry) {
        anyhow::bail!("entry_channel {entry} out of range [10, 30]");
    }
    if !(5..=15).contains(&exit) {
        anyhow::bail!("exit_channel {exit} out of range [5, 15]");
    }
    if !(0.0..=0.10).contains(&sl) || sl <= 0.0 {
        anyhow::bail!("sl_pct {sl} out of range (0.0, 0.10]");
    }
    if !(0.50..=1.0).contains(&alloc) {
        anyhow::bail!("allocation_pct {alloc} out of range [0.50, 1.0]");
    }
    if !(20..=100).contains(&baseline) {
        anyhow::bail!("atr_baseline_bars {baseline} out of range [20, 100]");
    }
    if exit >= entry {
        anyhow::bail!("exit_channel ({exit}) must be < entry_channel ({entry})");
    }
    Ok(())
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

/// Attempt to retrieve recent trade context from Vegapunk.
/// Returns `None` on failure (non-fatal — the batch continues without it).
async fn fetch_vegapunk_context(
    vegapunk: Option<&Arc<Mutex<VegapunkClient>>>,
    stats: &WeeklyStats,
) -> Option<String> {
    let vp = vegapunk?;
    let mut client = vp.lock().await;

    let query = format!(
        "過去7日間のトレード結果と学習: 総トレード数={}, 戦略別勝率を分析してください",
        stats.total_trades
    );
    match client.search(&query, "hybrid", 5).await {
        Ok(response) => {
            let context = response
                .results
                .into_iter()
                .filter_map(|item| item.text)
                .collect::<Vec<_>>()
                .join("\n---\n");
            if context.is_empty() {
                None
            } else {
                Some(context)
            }
        }
        Err(err) => {
            tracing::warn!("weekly_batch: Vegapunk search failed: {err:#}");
            None
        }
    }
}

/// Build the Gemini prompt from gathered stats, Wilson analysis, optional
/// Vegapunk context, and the current parameter blob.
fn build_gemini_prompt(
    stats: &WeeklyStats,
    vp_context: Option<&str>,
    current_params: &serde_json::Value,
    wilson: &[RegimeAnalysis],
) -> String {
    let mut prompt = String::with_capacity(2048);

    prompt.push_str(
        "あなたは自動売買システムのパラメータ最適化エキスパートです。\
         以下のデータを分析し、戦略パラメータの更新提案をJSON形式のみで返してください。\
         JSON以外のテキストは一切含めないこと。\n\n",
    );

    // Weekly trade stats section
    prompt.push_str("## 過去7日間のトレード統計\n");
    prompt.push_str(&format!("総トレード数: {}\n", stats.total_trades));
    prompt.push_str("戦略別集計:\n");
    for (strategy, trades, wins, avg_pnl) in &stats.by_strategy {
        let win_rate = if *trades > 0 {
            *wins as f64 / *trades as f64 * 100.0
        } else {
            0.0
        };
        prompt.push_str(&format!(
            "  - {strategy}: {trades}トレード, 勝率={win_rate:.1}%, 平均損益={avg_pnl:.4}\n"
        ));
    }

    // Wilson Score section
    prompt.push_str("\n## レジーム別 Wilson Score 分析 (95%信頼区間下限)\n");
    if wilson.is_empty() {
        prompt.push_str("データなし\n");
    } else {
        for analysis in wilson {
            let win_rate = if analysis.trades > 0 {
                analysis.wins as f64 / analysis.trades as f64 * 100.0
            } else {
                0.0
            };
            prompt.push_str(&format!(
                "  - {}: {}トレード, 勝率={:.1}%, Wilson下限={:.4}\n",
                analysis.regime, analysis.trades, win_rate, analysis.wilson_lb
            ));
        }
    }

    // Vegapunk context section
    if let Some(context) = vp_context {
        prompt.push_str("\n## Vegapunk 学習コンテキスト\n");
        prompt.push_str(context);
        prompt.push('\n');
    }

    // Current params section
    prompt.push_str("\n## 現在のパラメータ\n");
    prompt.push_str(&current_params.to_string());
    prompt.push('\n');

    // Instructions
    prompt.push_str(
        "\n## 指示\n\
         上記データを踏まえ、`donchian_trend_evolve_v1` 戦略の最適なパラメータを提案してください。\
         パラメータキー: entry_channel (整数), exit_channel (整数), sl_pct (小数), \
         allocation_pct (0.0〜1.0), atr_baseline_bars (整数)。\n\
         以下のJSON形式のみで応答すること:\n\
         {\"params\":{...},\"rationale\":\"変更理由\",\"expected_effect\":\"期待効果\"}\n",
    );

    prompt
}

/// Call the Gemini API and parse the `GeminiProposal` from the response text.
///
/// Returns a fallback proposal with unchanged params when:
/// - `api_key` is empty (Gemini disabled in config)
/// - The HTTP call fails
/// - The response cannot be parsed as `GeminiProposal`
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
        let stats = WeeklyStats {
            total_trades: 42,
            by_strategy: vec![("donchian_trend_evolve_v1".to_string(), 42, 28, 150.5)],
        };
        let wilson = vec![RegimeAnalysis {
            regime: "trending".to_string(),
            trades: 20,
            wins: 15,
            wilson_lb: 0.55,
        }];
        let params = serde_json::json!({"entry_channel": 20});

        let prompt = build_gemini_prompt(&stats, Some("vp context text"), &params, &wilson);

        assert!(prompt.contains("42"));
        assert!(prompt.contains("donchian_trend_evolve_v1"));
        assert!(prompt.contains("trending"));
        assert!(prompt.contains("Wilson"));
        assert!(prompt.contains("vp context text"));
        assert!(prompt.contains("entry_channel"));
    }

    #[test]
    fn build_gemini_prompt_no_vegapunk_context() {
        let stats = WeeklyStats {
            total_trades: 0,
            by_strategy: vec![],
        };
        let wilson: Vec<RegimeAnalysis> = vec![];
        let params = serde_json::json!({});

        let prompt = build_gemini_prompt(&stats, None, &params, &wilson);

        // Without context the Vegapunk section should be absent
        assert!(!prompt.contains("Vegapunk 学習コンテキスト"));
        // But prompt must still include output format instructions
        assert!(prompt.contains("GeminiProposal") || prompt.contains("rationale"));
    }
}
