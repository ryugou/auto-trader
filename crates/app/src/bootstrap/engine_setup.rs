use std::sync::Arc;

use auto_trader_core::config::AppConfig;
use auto_trader_core::event::SignalEvent;
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_core::types::Pair;
use auto_trader_market::monitor::MarketMonitor;
use auto_trader_market::price_store::FeedKey;
use auto_trader_strategy::engine::StrategyEngine;
use tokio::sync::mpsc;

use super::{feeds, fx_monitor, knowledge_store, strategies};
use crate::runtime::warmup::{self, BitflyerCandleSeed};

pub(crate) struct EngineSetup {
    pub(crate) expected_feeds: Vec<FeedKey>,
    pub(crate) fx_monitor: Option<MarketMonitor>,
    pub(crate) knowledge_store: Option<Arc<dyn KnowledgeStore>>,
    pub(crate) engine: StrategyEngine,
    pub(crate) bitflyer_seed: BitflyerCandleSeed,
    pub(crate) registered_strategies: Vec<String>,
}

pub(crate) async fn build(
    config: &AppConfig,
    pool: &sqlx::PgPool,
    signal_tx: mpsc::Sender<SignalEvent>,
) -> anyhow::Result<EngineSetup> {
    let mut expected_feeds: Vec<FeedKey> = Vec::new();

    let fx_monitor = fx_monitor::build(config, pool, super::FX_TIMEFRAME, &mut expected_feeds)?;
    let knowledge_store = knowledge_store::connect(&config.vegapunk).await;
    let mut engine = strategies::build_engine(config, pool, &knowledge_store, signal_tx).await?;
    strategies::warn_catalog_drift(pool, &engine).await;

    let crypto_pairs_for_warmup: Vec<Pair> = feeds::resolve_configured_crypto_pairs(config);
    let fx_pairs_for_warmup: Vec<Pair> = feeds::resolve_configured_fx_pairs(config);
    let bitflyer_seed = warmup::feed_history(
        pool,
        &mut engine,
        &crypto_pairs_for_warmup,
        &fx_pairs_for_warmup,
        warmup::WarmupTimeframes {
            crypto: super::CRYPTO_TIMEFRAME,
            h1: super::H1_TIMEFRAME,
            fx: super::FX_TIMEFRAME,
            limit: super::WARMUP_LIMIT,
        },
    )
    .await;

    let registered_strategies = strategies::collect_registered_names(&engine);

    Ok(EngineSetup {
        expected_feeds,
        fx_monitor,
        knowledge_store,
        engine,
        bitflyer_seed,
        registered_strategies,
    })
}
