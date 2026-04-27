//! GMO Coin FX Market Feed — REST polling ticker → CandleBuilder.
//!
//! Uses the Public API (no auth required) to poll bid/ask prices
//! at a fixed 5-second interval, then builds M5 + H1 candles via
//! CandleBuilder — same pattern as BitflyerMonitor.

use crate::candle_builder::CandleBuilder;
use crate::market_feed::MarketFeed;
use crate::price_store::{FeedKey, LatestTick, PriceStore};
use async_trait::async_trait;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const POLL_INTERVAL_SECS: u64 = 5;
const BASE_URL: &str = "https://forex-api.coin.z.com/public";

#[derive(Debug, Deserialize)]
struct TickerResponse {
    status: i32,
    data: Vec<TickerData>,
}

#[derive(Debug, Deserialize)]
struct TickerData {
    symbol: String,
    ask: String,
    bid: String,
    timestamp: String,
    status: String,
}

pub struct GmoFxFeed {
    pairs: Vec<Pair>,
    pool: Option<PgPool>,
    primary_timeframe: String,
}

impl GmoFxFeed {
    pub fn new(pairs: Vec<Pair>, primary_timeframe: &str) -> Self {
        Self {
            pairs,
            pool: None,
            primary_timeframe: primary_timeframe.to_string(),
        }
    }

    pub fn with_db(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }
}

#[async_trait]
impl MarketFeed for GmoFxFeed {
    async fn run(
        self: Box<Self>,
        price_store: Arc<PriceStore>,
        price_tx: mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()> {
        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        // Build CandleBuilders per pair — primary timeframe (M5) + H1 secondary.
        // Only create H1 builders when the primary is not already H1 to avoid
        // duplicate events (same pattern as BitflyerMonitor).
        let mut builders: HashMap<String, CandleBuilder> = HashMap::new();
        let mut h1_builders: HashMap<String, CandleBuilder> = HashMap::new();
        for pair in &self.pairs {
            builders.insert(
                pair.0.clone(),
                CandleBuilder::new(
                    pair.clone(),
                    Exchange::GmoFx,
                    self.primary_timeframe.clone(),
                ),
            );
            if self.primary_timeframe != "H1" {
                h1_builders.insert(
                    pair.0.clone(),
                    CandleBuilder::new(pair.clone(), Exchange::GmoFx, "H1".to_string()),
                );
            }
        }

        // Precompute Pair + FeedKey per symbol to avoid per-tick allocations.
        let pair_map: HashMap<String, (Pair, FeedKey)> = self
            .pairs
            .iter()
            .map(|p| {
                let key = FeedKey::new(Exchange::GmoFx, p.clone());
                (p.0.clone(), (p.clone(), key))
            })
            .collect();
        let symbols: std::collections::HashSet<String> = pair_map.keys().cloned().collect();

        let mut interval = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_SECS));

        loop {
            interval.tick().await;

            if price_tx.is_closed() {
                tracing::info!("GMO FX feed: price channel closed, stopping");
                return Ok(());
            }

            // Poll the public ticker endpoint (no auth required).
            let resp = match client.get(format!("{BASE_URL}/v1/ticker")).send().await {
                Ok(r) => match r.error_for_status() {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("GMO FX ticker HTTP error: {e}");
                        continue;
                    }
                },
                Err(e) => {
                    tracing::warn!("GMO FX ticker poll failed: {e}");
                    continue;
                }
            };

            let ticker: TickerResponse = match resp.json().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("GMO FX ticker parse failed: {e}");
                    continue;
                }
            };

            if ticker.status != 0 {
                tracing::warn!("GMO FX ticker non-zero status: {}", ticker.status);
                continue;
            }

            for item in &ticker.data {
                if !symbols.contains(&item.symbol) {
                    continue;
                }
                if item.status != "OPEN" {
                    // Market closed (weekend / holiday) — flush any in-progress candle
                    // so it doesn't linger until next market open (could be days for weekend).
                    let now = chrono::Utc::now();
                    if let Some(builder) = builders.get_mut(&item.symbol)
                        && let Some(candle) = builder.try_complete(now, None, None)
                    {
                        if let Some(pool) = &self.pool
                            && let Err(e) =
                                auto_trader_db::candles::upsert_candle(pool, &candle).await
                        {
                            tracing::warn!("GMO FX: failed to save candle on market close: {e}");
                        }
                        let event = PriceEvent {
                            pair: candle.pair.clone(),
                            exchange: Exchange::GmoFx,
                            timestamp: candle.timestamp,
                            candle,
                            indicators: HashMap::new(),
                        };
                        if price_tx.send(event).await.is_err() {
                            tracing::info!(
                                "GMO FX feed: price channel closed during M5 flush, stopping"
                            );
                            return Ok(());
                        }
                    }
                    if let Some(h1_builder) = h1_builders.get_mut(&item.symbol)
                        && let Some(h1_candle) = h1_builder.try_complete(now, None, None)
                    {
                        if let Some(pool) = &self.pool
                            && let Err(e) =
                                auto_trader_db::candles::upsert_candle(pool, &h1_candle).await
                        {
                            tracing::warn!("GMO FX: failed to save H1 candle on market close: {e}");
                        }
                        let h1_event = PriceEvent {
                            pair: h1_candle.pair.clone(),
                            exchange: Exchange::GmoFx,
                            timestamp: h1_candle.timestamp,
                            candle: h1_candle,
                            indicators: HashMap::new(),
                        };
                        if price_tx.send(h1_event).await.is_err() {
                            tracing::info!(
                                "GMO FX feed: price channel closed during H1 flush, stopping"
                            );
                            return Ok(());
                        }
                    }
                    continue;
                }

                let bid = match Decimal::from_str(&item.bid) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "GMO FX: bid parse error for {}: '{}' — {e}",
                            item.symbol,
                            item.bid
                        );
                        continue;
                    }
                };
                let ask = match Decimal::from_str(&item.ask) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "GMO FX: ask parse error for {}: '{}' — {e}",
                            item.symbol,
                            item.ask
                        );
                        continue;
                    }
                };
                let ts = match chrono::DateTime::parse_from_rfc3339(&item.timestamp) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "GMO FX: timestamp parse error for {}: '{}' — {e}",
                            item.symbol,
                            item.timestamp
                        );
                        continue;
                    }
                };
                let ts = ts.with_timezone(&chrono::Utc);
                let mid = (bid + ask) / Decimal::from(2);
                let (_pair, feed_key) = &pair_map[&item.symbol];
                price_store
                    .update(
                        feed_key.clone(),
                        LatestTick {
                            price: mid,
                            best_bid: Some(bid),
                            best_ask: Some(ask),
                            ts,
                        },
                    )
                    .await;

                // Feed into primary-timeframe CandleBuilder (e.g. M5).
                // Volume is not provided by the GMO ticker, so pass zero.
                if let Some(builder) = builders.get_mut(&item.symbol)
                    && let Some(candle) =
                        builder.on_tick(mid, Decimal::ZERO, ts, Some(bid), Some(ask))
                {
                    if let Some(pool) = &self.pool
                        && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &candle).await
                    {
                        tracing::warn!("GMO FX: failed to save candle: {e}");
                    }
                    let event = PriceEvent {
                        pair: candle.pair.clone(),
                        exchange: Exchange::GmoFx,
                        timestamp: candle.timestamp,
                        candle,
                        indicators: HashMap::new(),
                    };
                    if price_tx.send(event).await.is_err() {
                        tracing::info!("GMO FX feed: price channel closed, stopping");
                        return Ok(());
                    }
                }

                // Feed into H1 CandleBuilder (when primary != H1).
                if let Some(h1_builder) = h1_builders.get_mut(&item.symbol)
                    && let Some(h1_candle) =
                        h1_builder.on_tick(mid, Decimal::ZERO, ts, Some(bid), Some(ask))
                {
                    if let Some(pool) = &self.pool
                        && let Err(e) =
                            auto_trader_db::candles::upsert_candle(pool, &h1_candle).await
                    {
                        tracing::warn!("GMO FX: failed to save H1 candle: {e}");
                    }
                    let h1_event = PriceEvent {
                        pair: h1_candle.pair.clone(),
                        exchange: Exchange::GmoFx,
                        timestamp: h1_candle.timestamp,
                        candle: h1_candle,
                        indicators: HashMap::new(),
                    };
                    if price_tx.send(h1_event).await.is_err() {
                        tracing::info!("GMO FX feed: price channel closed, stopping");
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::{Exchange, Pair};

    #[test]
    fn gmo_fx_feed_new_has_no_pool() {
        let pairs = vec![Pair::new("USD_JPY")];
        let feed = GmoFxFeed::new(pairs.clone(), "M5");
        assert_eq!(feed.pairs, pairs);
        assert_eq!(feed.primary_timeframe, "M5");
        assert!(feed.pool.is_none());
    }

    #[test]
    fn exchange_gmo_fx_as_str() {
        assert_eq!(Exchange::GmoFx.as_str(), "gmo_fx");
    }
}
