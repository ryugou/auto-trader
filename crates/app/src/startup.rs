use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context as _;
use auto_trader_core::config::{AppConfig, GeminiConfig, StrategyConfig};
use auto_trader_core::knowledge::KnowledgeStore;
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
    let mut missing: Vec<&str> = required
        .iter()
        .filter(|ex| !map.contains_key(*ex))
        .map(|ex| ex.as_str())
        .collect();
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
    reject_non_positive_margin_for_exchanges_in_use(&required, &map)?;
    Ok(map)
}

fn reject_non_positive_margin_for_exchanges_in_use(
    required: &HashSet<Exchange>,
    map: &HashMap<Exchange, Decimal>,
) -> anyhow::Result<()> {
    for ex in required {
        let value = map
            .get(ex)
            .expect("required entries are present after the missing check");
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
                            strategy_name = %sc.name,
                            "gemini api key not set or empty, skipping strategy"
                        );
                        continue;
                    }
                };
                let gemini = match gemini_config {
                    Some(c) => c,
                    None => {
                        tracing::warn!(strategy_name = %sc.name, "gemini config missing, skipping strategy");
                        continue;
                    }
                };

                let store = match knowledge {
                    Some(s) => Arc::clone(s),
                    None => {
                        tracing::warn!(strategy_name = %sc.name, "knowledge_store unavailable, skipping strategy");
                        continue;
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
                engine.add_strategy(Box::new(strategy), sc.mode.clone());
                log_strategy_registered(&sc.name, &sc.mode);
            }
            name if name.starts_with("bb_mean_revert") => {
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                engine.add_strategy(
                    Box::new(auto_trader_strategy::bb_mean_revert::BbMeanRevertV1::new(
                        sc.name.clone(),
                        pairs,
                    )),
                    sc.mode.clone(),
                );
                log_strategy_registered(&sc.name, &sc.mode);
            }
            name if name.starts_with("donchian_trend_evolve") => {
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                let params: serde_json::Value = match sqlx::query_scalar::<
                    _,
                    sqlx::types::Json<serde_json::Value>,
                >(
                    "SELECT params FROM strategy_params WHERE strategy_name = $1",
                )
                .bind(&sc.name)
                .fetch_optional(pool)
                .await
                {
                    Ok(Some(j)) => j.0,
                    Ok(None) => {
                        tracing::warn!(strategy_name = %sc.name, "no strategy_params row, using defaults");
                        serde_json::json!({
                            "entry_channel": 20, "exit_channel": 10, "atr_baseline_bars": 50
                        })
                    }
                    Err(e) => {
                        tracing::warn!(
                            strategy_name = %sc.name,
                            error = %e,
                            "failed to load strategy_params, using defaults"
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
                log_strategy_registered(&sc.name, &sc.mode);
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
                log_strategy_registered(&sc.name, &sc.mode);
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
                log_strategy_registered(&sc.name, &sc.mode);
            }
            name if is_fx_new_strategy(name) => {
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                engine.add_strategy(
                    Box::new(auto_trader_strategy::fx_new::FxNewV1::new(
                        sc.name.clone(),
                        pairs,
                    )),
                    sc.mode.clone(),
                );
                log_strategy_registered(&sc.name, &sc.mode);
            }
            other => {
                tracing::warn!(strategy_name = %other, "unknown strategy, skipping");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
