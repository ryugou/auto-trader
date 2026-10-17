use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context as _;
use auto_trader_core::config::{AppConfig, GeminiConfig, StrategyConfig};
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_strategy::engine::StrategyEngine;
use rust_decimal::Decimal;
use sqlx::PgPool;

pub fn resolve_exchange_liquidation_levels(
    accounts: &[TradingAccount],
    config: &AppConfig,
) -> anyhow::Result<HashMap<Exchange, Decimal>> {
    let mut required: HashSet<Exchange> = HashSet::new();
    for acct in accounts {
        match acct.exchange.parse::<Exchange>() {
            Ok(ex) => {
                required.insert(ex);
            }
            Err(e) => {
                anyhow::bail!(
                    "failed to parse exchange for trading_accounts row {}: {e}",
                    acct.id
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
                anyhow::bail!("invalid option \"exchange_margin.{key}\": {e}");
            }
        }
    }
    let mut missing: Vec<&str> = Vec::new();
    let mut in_use: Vec<(Exchange, Decimal)> = Vec::new();
    for ex in &required {
        match map.get(ex) {
            Some(value) => in_use.push((*ex, *value)),
            None => missing.push(ex.as_str()),
        }
    }
    missing.sort();
    if !missing.is_empty() {
        let quoted_missing: Vec<String> = missing
            .iter()
            .map(|name| format!("\"exchange_margin.{name}\""))
            .collect();
        anyhow::bail!(
            "invalid option {}: missing for active accounts; add [exchange_margin.<name>] liquidation_margin_level = <value> for each",
            quoted_missing.join(", ")
        );
    }
    reject_non_positive_margin_for_exchanges_in_use(&in_use)?;
    Ok(map)
}

fn reject_non_positive_margin_for_exchanges_in_use(
    in_use: &[(Exchange, Decimal)],
) -> anyhow::Result<()> {
    for (ex, value) in in_use {
        if *value <= Decimal::ZERO {
            anyhow::bail!(
                "invalid option \"exchange_margin.{}.liquidation_margin_level\": must be positive, got {value}",
                ex.as_str()
            );
        }
    }
    Ok(())
}

pub fn liquidation_level_or_log<F>(
    map: &HashMap<Exchange, Decimal>,
    exchange: Exchange,
    context: F,
) -> Option<Decimal>
where
    F: FnOnce() -> String,
{
    match map.get(&exchange).copied() {
        Some(y) if y > Decimal::ZERO => Some(y),
        Some(y) => {
            tracing::error!(
                exchange = %exchange,
                liquidation_margin_level = %y,
                context = %context(),
                "liquidation_margin_level is non-positive, skipping; fix the exchange_margin config and restart, or recreate the account via the API which validates this constraint"
            );
            None
        }
        None => {
            tracing::error!(
                exchange = %exchange,
                context = %context(),
                "liquidation_margin_level entry missing, skipping; this indicates a runtime account was created without an exchange_margin entry, which the API should now reject"
            );
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FxNewMismatchKind {
    Leverage {
        account_leverage: Decimal,
        expected_leverage: Decimal,
    },
    Exchange {
        expected_exchange: Exchange,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FxNewAccountMismatch {
    pub account_name: String,
    pub account_exchange: String,
    pub kind: FxNewMismatchKind,
}

fn is_fx_new_strategy(strategy_name: &str) -> bool {
    strategy_name.starts_with("fx_new")
}

pub fn check_fx_new_account_assumptions(accounts: &[TradingAccount]) -> Vec<FxNewAccountMismatch> {
    let mut mismatches = Vec::new();
    for a in accounts
        .iter()
        .filter(|a| a.active && is_fx_new_strategy(&a.strategy))
    {
        if a.leverage != auto_trader_strategy::fx_new::GMO_FX_LEVERAGE {
            mismatches.push(FxNewAccountMismatch {
                account_name: a.name.clone(),
                account_exchange: a.exchange.clone(),
                kind: FxNewMismatchKind::Leverage {
                    account_leverage: a.leverage,
                    expected_leverage: auto_trader_strategy::fx_new::GMO_FX_LEVERAGE,
                },
            });
        }
        let exchange_matches = a
            .exchange
            .parse::<Exchange>()
            .is_ok_and(|ex| ex == auto_trader_strategy::fx_new::FX_NEW_EXCHANGE);
        if !exchange_matches {
            mismatches.push(FxNewAccountMismatch {
                account_name: a.name.clone(),
                account_exchange: a.exchange.clone(),
                kind: FxNewMismatchKind::Exchange {
                    expected_exchange: auto_trader_strategy::fx_new::FX_NEW_EXCHANGE,
                },
            });
        }
    }
    mismatches
}

pub fn fx_new_dispatch_allowed(strategy: &str, account_exchange: Exchange) -> bool {
    !is_fx_new_strategy(strategy)
        || account_exchange == auto_trader_strategy::fx_new::FX_NEW_EXCHANGE
}

pub fn effective_dry_run(account_type: &str, live_forces_dry_run: bool) -> bool {
    account_type == "paper" || live_forces_dry_run
}

fn log_strategy_registered(name: &str, mode: &str) {
    tracing::info!(strategy_name = name, mode = mode, "strategy registered");
}

pub async fn register_strategies(
    engine: &mut StrategyEngine,
    strategies: &[StrategyConfig],
    pool: &PgPool,
    knowledge: &Option<Arc<dyn KnowledgeStore>>,
    gemini_config: Option<&GeminiConfig>,
) -> anyhow::Result<()> {
    for sc in strategies {
        if !sc.enabled {
            continue;
        }
        register_one_strategy(engine, sc, pool, knowledge, gemini_config).await?;
    }
    Ok(())
}

async fn register_one_strategy(
    engine: &mut StrategyEngine,
    sc: &StrategyConfig,
    pool: &PgPool,
    knowledge: &Option<Arc<dyn KnowledgeStore>>,
    gemini_config: Option<&GeminiConfig>,
) -> anyhow::Result<()> {
    match sc.name.as_str() {
        name if name.starts_with("swing_llm") => {
            if let Some(strategy) = build_swing_llm(sc, knowledge, gemini_config)? {
                add_strategy_and_log(engine, sc, strategy);
            }
        }
        name if name.starts_with("bb_mean_revert") => {
            add_strategy_and_log(
                engine,
                sc,
                Box::new(auto_trader_strategy::bb_mean_revert::BbMeanRevertV1::new(
                    sc.name.clone(),
                    collect_pairs(sc),
                )),
            );
        }
        name if name.starts_with("donchian_trend_evolve") => {
            let params = load_donchian_evolve_params(pool, &sc.name).await;
            add_strategy_and_log(
                engine,
                sc,
                Box::new(
                    auto_trader_strategy::donchian_trend_evolve::DonchianTrendEvolveV1::new(
                        sc.name.clone(),
                        collect_pairs(sc),
                        params,
                    ),
                ),
            );
        }
        name if name.starts_with("donchian_trend") => {
            add_strategy_and_log(
                engine,
                sc,
                Box::new(auto_trader_strategy::donchian_trend::DonchianTrendV1::new(
                    sc.name.clone(),
                    collect_pairs(sc),
                )),
            );
        }
        name if name.starts_with("squeeze_momentum") => {
            add_strategy_and_log(
                engine,
                sc,
                Box::new(
                    auto_trader_strategy::squeeze_momentum::SqueezeMomentumV1::new(
                        sc.name.clone(),
                        collect_pairs(sc),
                    ),
                ),
            );
        }
        name if is_fx_new_strategy(name) => {
            add_strategy_and_log(
                engine,
                sc,
                Box::new(auto_trader_strategy::fx_new::FxNewV1::new(
                    sc.name.clone(),
                    collect_pairs(sc),
                )),
            );
        }
        other => {
            tracing::warn!(strategy_name = %other, "unknown strategy, skipping");
        }
    }
    Ok(())
}

fn build_swing_llm(
    sc: &StrategyConfig,
    knowledge: &Option<Arc<dyn KnowledgeStore>>,
    gemini_config: Option<&GeminiConfig>,
) -> anyhow::Result<Option<Box<dyn Strategy>>> {
    let holding_days_max = sc
        .params
        .get("holding_days_max")
        .and_then(|v| v.as_integer())
        .unwrap_or(14) as u32;
    let pairs = collect_pairs(sc);

    let gemini_api_key = match std::env::var("GEMINI_API_KEY") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            tracing::warn!(
                strategy_name = %sc.name,
                "gemini api key not set or empty, skipping strategy"
            );
            return Ok(None);
        }
    };
    let gemini = match gemini_config {
        Some(c) => c,
        None => {
            tracing::warn!(strategy_name = %sc.name, "gemini config missing, skipping strategy");
            return Ok(None);
        }
    };

    let store = match knowledge {
        Some(s) => Arc::clone(s),
        None => {
            tracing::warn!(strategy_name = %sc.name, "knowledge_store unavailable, skipping strategy");
            return Ok(None);
        }
    };

    let strategy = auto_trader_strategy::swing_llm::SwingLLMv1::new(
        sc.name.clone(),
        pairs,
        holding_days_max,
        store,
        gemini.api_url.clone(),
        gemini_api_key,
        gemini.model.clone(),
    )
    .with_context(|| format!("failed to build strategy {}", sc.name))?;
    Ok(Some(Box::new(strategy)))
}

async fn load_donchian_evolve_params(pool: &PgPool, strategy_name: &str) -> serde_json::Value {
    match sqlx::query_scalar::<_, sqlx::types::Json<serde_json::Value>>(
        "SELECT params FROM strategy_params WHERE strategy_name = $1",
    )
    .bind(strategy_name)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(j)) => j.0,
        Ok(None) => {
            tracing::warn!(strategy_name = %strategy_name, "no strategy_params row, using defaults");
            build_default_donchian_evolve_params()
        }
        Err(e) => {
            tracing::warn!(
                strategy_name = %strategy_name,
                error = %e,
                "failed to load strategy_params, using defaults"
            );
            build_default_donchian_evolve_params()
        }
    }
}

fn build_default_donchian_evolve_params() -> serde_json::Value {
    serde_json::json!({
        "entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50
    })
}

fn collect_pairs(sc: &StrategyConfig) -> Vec<Pair> {
    sc.pairs.iter().map(|s| Pair::new(s)).collect()
}

fn add_strategy_and_log(
    engine: &mut StrategyEngine,
    sc: &StrategyConfig,
    strategy: Box<dyn Strategy>,
) {
    engine.add_strategy(strategy, sc.mode.clone());
    log_strategy_registered(&sc.name, &sc.mode);
}

#[cfg(test)]
mod tests;
