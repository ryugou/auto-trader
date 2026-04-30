use auto_trader_core::config::{GeminiConfig, StrategyConfig};
use auto_trader_core::types::Pair;
use auto_trader_strategy::engine::StrategyEngine;
use auto_trader_vegapunk::client::VegapunkClient;
use sqlx::PgPool;

/// Register all enabled strategies from config into the engine.
///
/// This function iterates `config.strategies`, constructs each strategy
/// instance, and adds it to the engine via `engine.add_strategy`.
/// Strategies that require external resources (Vegapunk, Gemini, DB params)
/// are skipped with a warning if those resources are unavailable.
pub async fn register_strategies(
    engine: &mut StrategyEngine,
    strategies: &[StrategyConfig],
    pool: &PgPool,
    vegapunk_base: &Option<VegapunkClient>,
    vegapunk_schema: &str,
    gemini_config: Option<&GeminiConfig>,
) {
    for sc in strategies {
        if !sc.enabled {
            continue;
        }
        match sc.name.as_str() {
            name if name.starts_with("swing_llm") => {
                let holding_days_max = sc
                    .params
                    .get("holding_days_max")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(14) as u32;
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();

                let gemini_api_key = match std::env::var("GEMINI_API_KEY") {
                    Ok(value) if !value.trim().is_empty() => value,
                    _ => {
                        tracing::warn!(
                            "GEMINI_API_KEY not set or empty, skipping strategy: {}",
                            sc.name
                        );
                        continue;
                    }
                };
                let gemini = match gemini_config {
                    Some(c) => c,
                    None => {
                        tracing::warn!("gemini config missing, skipping strategy: {}", sc.name);
                        continue;
                    }
                };

                // Clone from shared Vegapunk channel (no new TCP connection)
                let vp_client = match vegapunk_base {
                    Some(base) => {
                        VegapunkClient::clone_from_channel(base, vegapunk_schema)
                    }
                    None => {
                        tracing::warn!("vegapunk unavailable, skipping strategy: {}", sc.name);
                        continue;
                    }
                };

                engine.add_strategy(
                    Box::new(auto_trader_strategy::swing_llm::SwingLLMv1::new(
                        sc.name.clone(),
                        pairs,
                        holding_days_max,
                        vp_client,
                        gemini.api_url.clone(),
                        gemini_api_key,
                        gemini.model.clone(),
                    )),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            // Strategies with hardcoded Rust const parameters.
            // Tuning requires changing the const in the strategy source
            // and rebuilding. See config/default.toml SoT header.
            name if name.starts_with("bb_mean_revert") => {
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                engine.add_strategy(
                    Box::new(auto_trader_strategy::bb_mean_revert::BbMeanRevertV1::new(
                        sc.name.clone(),
                        pairs,
                    )),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            name if name.starts_with("donchian_trend_evolve") => {
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                let params: serde_json::Value =
                    sqlx::query_scalar::<_, sqlx::types::Json<serde_json::Value>>(
                        "SELECT params FROM strategy_params WHERE strategy_name = $1",
                    )
                    .bind(&sc.name)
                    .fetch_optional(pool)
                    .await
                    .unwrap_or(None)
                    .map(|j| j.0)
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50
                        })
                    });
                engine.add_strategy(
                    Box::new(
                        auto_trader_strategy::donchian_trend_evolve::DonchianTrendEvolveV1::new(
                            sc.name.clone(),
                            pairs,
                            params,
                        ),
                    ),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            name if name.starts_with("donchian_trend") => {
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                engine.add_strategy(
                    Box::new(auto_trader_strategy::donchian_trend::DonchianTrendV1::new(
                        sc.name.clone(),
                        pairs,
                    )),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            name if name.starts_with("squeeze_momentum") => {
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                engine.add_strategy(
                    Box::new(
                        auto_trader_strategy::squeeze_momentum::SqueezeMomentumV1::new(
                            sc.name.clone(),
                            pairs,
                        ),
                    ),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            other => {
                tracing::warn!("unknown strategy: {other}, skipping");
            }
        }
    }
}
