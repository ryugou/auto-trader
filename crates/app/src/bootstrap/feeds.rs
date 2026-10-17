use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use auto_trader_core::config::{AppConfig, FeedWatchdogConfig};
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_market::market_feed::MarketFeed;
use auto_trader_market::monitor::MarketMonitor;
use auto_trader_market::price_store::{FeedKey, PriceStore};
use auto_trader_notify::Notifier;
use tokio::sync::mpsc;

use crate::runtime::warmup::BitflyerCandleSeed;

pub(crate) struct FeedTimeframes<'a> {
    pub(crate) crypto: &'a str,
    pub(crate) fx: &'a str,
}

pub(crate) fn build(
    config: &AppConfig,
    pool: &sqlx::PgPool,
    fx_monitor: Option<MarketMonitor>,
    bitflyer_seed: &mut BitflyerCandleSeed,
    timeframes: FeedTimeframes<'_>,
    expected_feeds: &mut Vec<FeedKey>,
) -> HashMap<Exchange, Box<dyn MarketFeed>> {
    let mut feeds: HashMap<Exchange, Box<dyn MarketFeed>> = HashMap::new();

    if let Some(fx_monitor) = fx_monitor {
        feeds.insert(Exchange::Oanda, Box::new(fx_monitor));
    }

    register_bitflyer(
        config,
        pool,
        bitflyer_seed,
        timeframes.crypto,
        expected_feeds,
        &mut feeds,
    );
    register_gmo_fx(config, pool, timeframes.fx, expected_feeds, &mut feeds);

    feeds
}

fn register_bitflyer(
    config: &AppConfig,
    pool: &sqlx::PgPool,
    bitflyer_seed: &mut BitflyerCandleSeed,
    timeframe: &str,
    expected_feeds: &mut Vec<FeedKey>,
    feeds: &mut HashMap<Exchange, Box<dyn MarketFeed>>,
) {
    let Some(bf_config) = &config.bitflyer else {
        return;
    };
    let crypto_pairs = resolve_configured_crypto_pairs(config);
    if crypto_pairs.is_empty() {
        return;
    }
    push_expected_feeds(expected_feeds, Exchange::BitflyerCfd, &crypto_pairs);
    let bf_feed = auto_trader_market::bitflyer::BitflyerMonitor::new(
        &bf_config.ws_url,
        crypto_pairs,
        timeframe,
    )
    .with_db(pool.clone())
    .with_candle_seed(
        std::mem::take(&mut bitflyer_seed.highs),
        std::mem::take(&mut bitflyer_seed.lows),
        std::mem::take(&mut bitflyer_seed.closes),
    );
    feeds.insert(Exchange::BitflyerCfd, Box::new(bf_feed));
}

fn register_gmo_fx(
    config: &AppConfig,
    pool: &sqlx::PgPool,
    timeframe: &str,
    expected_feeds: &mut Vec<FeedKey>,
    feeds: &mut HashMap<Exchange, Box<dyn MarketFeed>>,
) {
    let gmo_fx_pairs: Vec<Pair> = crate::resolve_configured_fx_pairs(config);
    if gmo_fx_pairs.is_empty() {
        tracing::info!("no fx pairs configured, gmo fx feed disabled");
        return;
    }
    push_expected_feeds(expected_feeds, Exchange::GmoFx, &gmo_fx_pairs);
    let gmo_feed = auto_trader_market::gmo_fx::GmoFxFeed::new(gmo_fx_pairs.clone(), timeframe)
        .with_db(pool.clone());
    feeds.insert(Exchange::GmoFx, Box::new(gmo_feed));
    tracing::info!(pair_count = gmo_fx_pairs.len(), "gmo fx feed registered");
}

fn push_expected_feeds(expected_feeds: &mut Vec<FeedKey>, exchange: Exchange, pairs: &[Pair]) {
    for p in pairs {
        expected_feeds.push(FeedKey::new(exchange, p.clone()));
    }
}

pub(crate) fn group_pairs_by_exchange(
    expected_feeds: &[FeedKey],
) -> Arc<HashMap<Exchange, HashSet<String>>> {
    let mut map: HashMap<Exchange, HashSet<String>> = HashMap::new();
    for fk in expected_feeds {
        map.entry(fk.exchange)
            .or_default()
            .insert(fk.pair.0.clone());
    }
    Arc::new(map)
}

pub(crate) fn spawn_all(
    feeds: HashMap<Exchange, Box<dyn MarketFeed>>,
    price_store: &Arc<PriceStore>,
    price_tx: &mpsc::Sender<PriceEvent>,
    notifier: &Arc<Notifier>,
    watchdog: &FeedWatchdogConfig,
) -> Vec<tokio::task::AbortHandle> {
    let mut feed_handles: Vec<tokio::task::AbortHandle> = Vec::new();
    for (exchange, feed) in feeds {
        let feed_price_store = price_store.clone();
        let feed_price_tx = price_tx.clone();
        let exchange_label = exchange;
        let inner_handle: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
            tracing::info!(exchange = %exchange_label, "starting market feed");
            feed.run(feed_price_store, feed_price_tx).await
        });
        let abort_handle = auto_trader::feed_watchdog::spawn_feed_supervisor(
            exchange_label,
            notifier.clone(),
            inner_handle,
            watchdog.notify_retry_attempts,
            watchdog.notify_retry_initial_secs,
        );
        feed_handles.push(abort_handle);
    }
    feed_handles
}

pub(crate) fn resolve_configured_crypto_pairs(config: &AppConfig) -> Vec<Pair> {
    config
        .pairs
        .crypto
        .as_ref()
        .map(|v| v.iter().map(|s| Pair::new(s)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
