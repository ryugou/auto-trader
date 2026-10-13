use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Candle, Exchange, Pair};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::PgPool;
use tokio::sync::mpsc;

use crate::candle_builder::CandleBuilder;
use crate::market_feed::MarketFeed;
use crate::price_store::{FeedKey, LatestTick, PriceStore};

const POLL_INTERVAL_SECS: u64 = 5;
const BASE_URL: &str = "https://forex-api.coin.z.com/public";

#[derive(Debug, Deserialize)]
struct TickerResponse {
    status: i32,
    #[serde(default)]
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

const GMO_FX_STATUS_MAINTENANCE: i32 = 5;
const GMO_FX_TICKER_VOLUME_UNAVAILABLE: Decimal = Decimal::ZERO;

fn should_mark_market_closed(status: i32) -> bool {
    status == GMO_FX_STATUS_MAINTENANCE
}

async fn save_and_send_candle(
    pool: Option<&PgPool>,
    candle: Candle,
    price_tx: &mpsc::Sender<PriceEvent>,
) -> bool {
    if let Some(pool) = pool
        && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &candle).await
    {
        tracing::warn!(error = %e, timeframe = %candle.timeframe, "failed to save candle");
    }
    let event = PriceEvent {
        pair: candle.pair.clone(),
        exchange: Exchange::GmoFx,
        timestamp: candle.timestamp,
        candle,
        indicators: HashMap::new(),
    };
    if price_tx.send(event).await.is_err() {
        tracing::info!("price channel closed, stopping gmo fx feed");
        return false;
    }
    true
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
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;

            if price_tx.is_closed() {
                tracing::info!("price channel closed, stopping gmo fx feed");
                return Ok(());
            }

            let resp = match client.get(format!("{BASE_URL}/v1/ticker")).send().await {
                Ok(r) => match r.error_for_status() {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to call gmo fx ticker api: non-2xx response");
                        continue;
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "failed to call gmo fx ticker api: send failed");
                    continue;
                }
            };

            let ticker: TickerResponse = match resp.json().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to parse gmo fx ticker response");
                    continue;
                }
            };

            if ticker.status != 0 {
                if ticker.status == GMO_FX_STATUS_MAINTENANCE {
                    tracing::debug!(
                        status = ticker.status,
                        "gmo fx ticker reports maintenance, waiting"
                    );
                } else {
                    tracing::warn!(status = ticker.status, "invalid ticker status: non-zero");
                }
                if should_mark_market_closed(ticker.status) {
                    for (_, fk) in pair_map.values() {
                        price_store.mark_market_closed(fk.clone()).await;
                    }
                }
                continue;
            }

            for item in &ticker.data {
                if !symbols.contains(&item.symbol) {
                    continue;
                }
                if item.status != "OPEN" {
                    let (_pair, feed_key) = &pair_map[&item.symbol];
                    price_store.mark_market_closed(feed_key.clone()).await;
                    let now = chrono::Utc::now();
                    if let Some(builder) = builders.get_mut(&item.symbol)
                        && let Some(candle) = builder.try_complete(now, None, None)
                        && !save_and_send_candle(self.pool.as_ref(), candle, &price_tx).await
                    {
                        return Ok(());
                    }
                    if let Some(h1_builder) = h1_builders.get_mut(&item.symbol)
                        && let Some(h1_candle) = h1_builder.try_complete(now, None, None)
                        && !save_and_send_candle(self.pool.as_ref(), h1_candle, &price_tx).await
                    {
                        return Ok(());
                    }
                    continue;
                }

                let bid = match Decimal::from_str(&item.bid) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            symbol = %item.symbol,
                            bid = %item.bid,
                            error = %e,
                            "failed to parse bid price"
                        );
                        continue;
                    }
                };
                let ask = match Decimal::from_str(&item.ask) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            symbol = %item.symbol,
                            ask = %item.ask,
                            error = %e,
                            "failed to parse ask price"
                        );
                        continue;
                    }
                };
                let ts = match chrono::DateTime::parse_from_rfc3339(&item.timestamp) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            symbol = %item.symbol,
                            timestamp = %item.timestamp,
                            error = %e,
                            "failed to parse timestamp"
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

                if let Some(builder) = builders.get_mut(&item.symbol)
                    && let Some(candle) = builder.on_tick(
                        mid,
                        GMO_FX_TICKER_VOLUME_UNAVAILABLE,
                        ts,
                        Some(bid),
                        Some(ask),
                    )
                    && !save_and_send_candle(self.pool.as_ref(), candle, &price_tx).await
                {
                    return Ok(());
                }

                if let Some(h1_builder) = h1_builders.get_mut(&item.symbol)
                    && let Some(h1_candle) = h1_builder.on_tick(
                        mid,
                        GMO_FX_TICKER_VOLUME_UNAVAILABLE,
                        ts,
                        Some(bid),
                        Some(ask),
                    )
                    && !save_and_send_candle(self.pool.as_ref(), h1_candle, &price_tx).await
                {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
