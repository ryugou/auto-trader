use auto_trader_core::config::{AppConfig, GeminiConfig, StrategyConfig};
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_strategy::engine::StrategyEngine;
use auto_trader_vegapunk::client::VegapunkClient;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};

/// Resolve broker liquidation thresholds per exchange from config, validated
/// against active trading accounts.
///
/// Returns `Err` if any active account's exchange lacks an
/// `[exchange_margin.<name>]` entry, or if a config key fails to parse as
/// `Exchange`. This is the fail-closed startup gate — running with an
/// unresolved exchange would let the position sizer pick a default and
/// silently miscompute position sizes for that broker.
pub async fn resolve_exchange_liquidation_levels(
    pool: &PgPool,
    config: &AppConfig,
) -> anyhow::Result<HashMap<Exchange, Decimal>> {
    let active_accounts = auto_trader_db::trading_accounts::list_all(pool).await?;
    let mut required: HashSet<Exchange> = HashSet::new();
    for acct in &active_accounts {
        match acct.exchange.parse::<Exchange>() {
            Ok(ex) => {
                required.insert(ex);
            }
            Err(e) => {
                anyhow::bail!(
                    "trading_accounts row {} has unrecognised exchange '{}': {e}",
                    acct.id,
                    acct.exchange
                );
            }
        }
    }
    let mut map: HashMap<Exchange, Decimal> = HashMap::new();
    for (key, cfg) in config.exchange_margin.iter() {
        match key.parse::<Exchange>() {
            Ok(ex) => {
                map.insert(ex, cfg.liquidation_margin_level);
            }
            Err(e) => {
                anyhow::bail!("config: [exchange_margin.{key}] is not a recognised exchange: {e}");
            }
        }
    }
    let missing: Vec<_> = required
        .iter()
        .filter(|ex| !map.contains_key(*ex))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "config: [exchange_margin.<name>] missing for active accounts: {:?}. \
             Add `liquidation_margin_level` for each.",
            missing
        );
    }
    Ok(map)
}

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
                    match sqlx::query_scalar::<_, sqlx::types::Json<serde_json::Value>>(
                        "SELECT params FROM strategy_params WHERE strategy_name = $1",
                    )
                    .bind(&sc.name)
                    .fetch_optional(pool)
                    .await
                    {
                        Ok(Some(j)) => j.0,
                        Ok(None) => {
                            tracing::warn!(
                                "no strategy_params row for '{}', using defaults",
                                sc.name
                            );
                            serde_json::json!({
                                "entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50
                            })
                        }
                        Err(e) => {
                            tracing::warn!(
                                "failed to load strategy_params for '{}': {e}, using defaults",
                                sc.name
                            );
                            serde_json::json!({
                                "entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50
                            })
                        }
                    };
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
