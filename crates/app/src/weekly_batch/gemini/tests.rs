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

    let prompt = build_gemini_prompt("donchian_trend_evolve_v1", &stats, None, &params, &wilson);

    assert!(!prompt.contains("過去トレード学習コンテキスト"));
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
    assert!(!prompt.contains("entry_channel"));
    assert!(prompt.contains("現在のパラメータ構造を維持"));
}

#[tokio::test]
async fn call_gemini_rejects_empty_api_key() {
    let err = call_gemini("http://unused.invalid", "", "model", "prompt")
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("api_key"),
        "error should mention api_key: {err}"
    );
}
