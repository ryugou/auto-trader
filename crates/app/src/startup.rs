use auto_trader_core::config::{AppConfig, GeminiConfig, StrategyConfig};
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_strategy::engine::StrategyEngine;
use auto_trader_vegapunk::client::VegapunkClient;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};

/// Resolve broker liquidation thresholds per exchange from config, validated
/// against active trading accounts.
///
/// Returns `Err` when:
/// - a config key (`[exchange_margin.<name>]`) fails to parse as `Exchange`,
/// - an active account's exchange has no matching config entry, or
/// - an active account's exchange has a `liquidation_margin_level` that is
///   not strictly positive.
///
/// Non-positive values on exchanges that have no active account are
/// **tolerated** — a stale TOML left at 0 for an unused exchange should not
/// brick startup. The `accounts::create` handler re-checks `> 0` when an
/// account is later created on such an exchange (defense in depth).
///
/// This is the startup half of a defense-in-depth fail-closed design:
/// - At startup: missing/invalid entries for exchanges in use abort the
///   process here, before any trading task spawns.
/// - At runtime: the API rejects new account creation for exchanges absent
///   from the resolved map or with a non-positive value
///   (`accounts::create`), and the worker tasks log + skip the affected
///   signal/exit/close instead of panicking when an entry is missing for an
///   in-flight trade. Together these prevent the position sizer from
///   running without a valid `liquidation_margin_level`.
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
    // Fail-closed: a non-positive `liquidation_margin_level` would let the
    // PositionSizer silently return None at runtime ("account too small")
    // because `1 / (Y + L*s)` with `Y <= 0` produces nonsense. Validate only
    // the entries actually used by an active account — entries for unused
    // exchanges may sit at 0 / negative without breaking this run, and we
    // don't want to crash startup over config a future operator left there.
    for ex in &required {
        let value = map.get(ex).expect("required entries are present after the missing check");
        if *value <= Decimal::ZERO {
            anyhow::bail!(
                "config: [exchange_margin.{}] liquidation_margin_level must be > 0, got {} \
                 (received non-positive value for exchange {:?})",
                ex.as_str(),
                value,
                ex
            );
        }
    }
    Ok(map)
}

/// Look up `liquidation_margin_level` for `exchange` in the resolved map, or
/// log a context-rich error and return `None`.
///
/// Worker tasks that need a sizing input call this on every iteration. The
/// startup gate (`resolve_exchange_liquidation_levels`) validates the map
/// against accounts at boot, and the create-account API rejects exchanges
/// not present in the map, but this helper is the runtime fallback if a row
/// snuck in another way (e.g. direct SQL). Returning `None` lets the caller
/// `continue` instead of panicking.
///
/// `context` is rendered into the log only on the miss path so the operator
/// can correlate the skip with a specific trade or signal. Pass a closure
/// that builds a terse identifier (e.g. `|| format!("close trade {id}")`)
/// — it runs at most once and only when the entry is missing.
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
            // Resolver tolerates non-positive entries on *unused* exchanges
            // so a stale TOML cannot brick startup, and the create-account
            // API rejects when an account is added against such an entry.
            // If a row still reaches here (e.g. created via direct SQL,
            // bypassing the API), treat the bad value as missing rather
            // than passing it to PositionSizer — which would surface as a
            // confusing "balance too small" error.
            tracing::error!(
                "exchange_liquidation_levels for {:?} is non-positive ({}) ({}) — skipping; \
                 fix the value in `[exchange_margin.{}]` and restart, or recreate the \
                 account via the API which validates this constraint",
                exchange,
                y,
                context(),
                exchange.as_str()
            );
            None
        }
        None => {
            tracing::error!(
                "exchange_liquidation_levels missing entry for {:?} ({}) — skipping; \
                 this indicates a runtime account was created without an [exchange_margin] \
                 entry, which the API should now reject",
                exchange,
                context()
            );
            None
        }
    }
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
