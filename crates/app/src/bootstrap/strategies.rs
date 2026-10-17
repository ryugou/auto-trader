use std::collections::HashSet;
use std::sync::Arc;

use auto_trader_core::config::AppConfig;
use auto_trader_core::event::SignalEvent;
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_strategy::engine::StrategyEngine;
use tokio::sync::mpsc;

pub(crate) async fn build_engine(
    config: &AppConfig,
    pool: &sqlx::PgPool,
    knowledge_store: &Option<Arc<dyn KnowledgeStore>>,
    signal_tx: mpsc::Sender<SignalEvent>,
) -> anyhow::Result<StrategyEngine> {
    let mut engine = StrategyEngine::new(signal_tx);
    auto_trader::startup::register_strategies(
        &mut engine,
        &config.strategies,
        pool,
        knowledge_store,
        config.gemini.as_ref(),
    )
    .await?;
    Ok(engine)
}

pub(crate) async fn warn_catalog_drift(pool: &sqlx::PgPool, engine: &StrategyEngine) {
    match auto_trader_db::strategies::list_strategy_names(pool).await {
        Ok(catalog_names) => {
            let registered = engine.registered_names();
            for name in find_missing_from_catalog(&registered, &catalog_names) {
                tracing::warn!(
                    strategy_name = %name,
                    remedy = "add a row to the strategies table (see migrations/20260407000003_strategies.sql)",
                    "invalid strategy: missing from strategies catalog"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to check strategy catalog drift");
        }
    }
}

fn find_missing_from_catalog<'a>(registered: &[&'a str], catalog: &[String]) -> Vec<&'a str> {
    let catalog_set: HashSet<&str> = catalog.iter().map(|s| s.as_str()).collect();
    registered
        .iter()
        .copied()
        .filter(|name| !catalog_set.contains(name))
        .collect()
}

pub(crate) fn collect_registered_names(engine: &StrategyEngine) -> Vec<String> {
    engine
        .registered_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests;
