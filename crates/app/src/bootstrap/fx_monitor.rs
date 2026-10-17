use auto_trader_core::config::{AppConfig, StrategyConfig};
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_market::monitor::MarketMonitor;
use auto_trader_market::oanda::OandaClient;
use auto_trader_market::price_store::FeedKey;

pub(crate) fn build(
    config: &AppConfig,
    pool: &sqlx::PgPool,
    timeframe: &str,
    expected_feeds: &mut Vec<FeedKey>,
) -> anyhow::Result<Option<MarketMonitor>> {
    let fx_pairs: Vec<Pair> = crate::resolve_configured_fx_pairs(config);
    if fx_pairs.is_empty() {
        tracing::info!("no fx pairs configured, fx monitor disabled");
        warn_if_fx_strategy_has_no_price_source(config);
        return Ok(None);
    }
    build_oanda_monitor(config, pool, timeframe, expected_feeds, fx_pairs)
}

fn build_oanda_monitor(
    config: &AppConfig,
    pool: &sqlx::PgPool,
    timeframe: &str,
    expected_feeds: &mut Vec<FeedKey>,
    fx_pairs: Vec<Pair>,
) -> anyhow::Result<Option<MarketMonitor>> {
    let (Some(api_key), Some(oanda_config)) =
        (super::resolve_oanda_api_key(), config.oanda.as_ref())
    else {
        tracing::info!(
            exchange = %Exchange::Oanda,
            "not configured or api key not set, fx monitor disabled"
        );
        return Ok(None);
    };
    let Some(account_id) = super::resolve_oanda_account_id(config) else {
        tracing::warn!(
            exchange = %Exchange::Oanda,
            "invalid oanda account id: not set"
        );
        return Ok(None);
    };
    for p in &fx_pairs {
        expected_feeds.push(FeedKey::new(Exchange::Oanda, p.clone()));
    }
    let oanda = OandaClient::new(&oanda_config.api_url, &account_id, &api_key)?;
    Ok(Some(
        MarketMonitor::new(oanda, fx_pairs, config.monitor.interval_secs, timeframe)
            .with_db(pool.clone()),
    ))
}

fn has_enabled_fx_strategy(strategies: &[StrategyConfig]) -> bool {
    strategies
        .iter()
        .any(|s| s.enabled && s.name.starts_with("swing_llm"))
}

fn warn_if_fx_strategy_has_no_price_source(config: &AppConfig) {
    if has_enabled_fx_strategy(&config.strategies) {
        tracing::warn!(
            remedy =
                "configure the oanda or gmo_fx feed, otherwise fx strategies receive no price data",
            "invalid fx price source: not configured"
        );
    }
}

#[cfg(test)]
mod tests;
