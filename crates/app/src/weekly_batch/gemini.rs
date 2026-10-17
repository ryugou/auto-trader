use anyhow::Context as _;

use super::{RegimeAnalysis, StrategyStats};

#[derive(Debug, serde::Deserialize)]
pub(super) struct GeminiProposal {
    pub(super) params: serde_json::Value,
    pub(super) rationale: String,
    #[serde(default)]
    pub(super) expected_effect: String,
}

pub(super) fn build_gemini_prompt(
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

pub(super) async fn call_gemini(
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
