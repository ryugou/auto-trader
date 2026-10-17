use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::types::{Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use tokio::sync::{RwLock, RwLockReadGuard};

/// Key that identifies the feed of a pair on an exchange.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedKey {
    /// Exchange of the feed.
    pub exchange: Exchange,
    /// Pair of the feed.
    pub pair: Pair,
}

impl FeedKey {
    /// Creates the key of the feed of `pair` on `exchange`.
    pub fn new(exchange: Exchange, pair: Pair) -> Self {
        Self { exchange, pair }
    }
}

/// Latest price observed on a feed.
#[derive(Debug, Clone)]
pub struct LatestTick {
    /// Price of the tick.
    pub price: Decimal,
    /// Best bid of the tick, if the feed gave one.
    pub best_bid: Option<Decimal>,
    /// Best ask of the tick, if the feed gave one.
    pub best_ask: Option<Decimal>,
    /// Time of the tick.
    pub ts: DateTime<Utc>,
}

/// Health status of a feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedStatus {
    /// The latest tick is not older than the stale threshold.
    Healthy,
    /// The latest tick is older than the stale threshold.
    Stale,
    /// No tick has been observed and the market is not marked closed.
    Missing,
    /// The market of the feed is marked closed.
    MarketClosed,
}

/// Health of one expected feed.
#[derive(Debug, Clone, Serialize)]
pub struct FeedHealth {
    /// Name of the exchange of the feed.
    pub exchange: String,
    /// Name of the pair of the feed.
    pub pair: String,
    /// Health status of the feed.
    pub status: FeedStatus,
    /// Age of the latest tick in seconds, or `None` if no tick has been observed.
    pub last_tick_age_secs: Option<i64>,
    /// Time at which the market was marked closed, or `None` if it is not marked closed.
    pub market_closed_at: Option<DateTime<Utc>>,
}

/// Age in seconds above which the latest tick of a feed counts as stale.
pub const STALE_THRESHOLD_SECS: i64 = 60;

/// Store of the latest tick of each feed together with the feeds that are expected and the feeds whose market is marked closed.
#[derive(Debug)]
pub struct PriceStore {
    latest: RwLock<HashMap<FeedKey, LatestTick>>,
    expected: Vec<FeedKey>,
    market_closed: RwLock<HashMap<FeedKey, DateTime<Utc>>>,
}

fn is_newer_than_stored(existing: Option<&LatestTick>, incoming_ts: DateTime<Utc>) -> bool {
    match existing {
        Some(t) => t.ts < incoming_ts,
        None => true,
    }
}

impl PriceStore {
    /// Creates a store that expects the feeds in `expected` and holds no tick.
    pub fn new(expected: Vec<FeedKey>) -> Arc<Self> {
        Arc::new(Self {
            latest: RwLock::new(HashMap::new()),
            expected,
            market_closed: RwLock::new(HashMap::new()),
        })
    }

    async fn clear_market_closed_on_tick(&self, key: &FeedKey) {
        self.market_closed.write().await.remove(key);
    }

    /// Stores `tick` as the latest tick of `key` if it is newer than the stored tick or none is stored, and clears the closed mark of `key` whether or not the tick is stored.
    pub async fn update(&self, key: FeedKey, tick: LatestTick) {
        self.clear_market_closed_on_tick(&key).await;
        let mut guard = self.latest.write().await;
        if is_newer_than_stored(guard.get(&key), tick.ts) {
            guard.insert(key, tick);
        }
    }

    /// Marks the market of `key` as closed at the current time.
    pub async fn mark_market_closed(&self, key: FeedKey) {
        self.market_closed.write().await.insert(key, Utc::now());
    }

    /// Returns the latest tick of `key`, or `None` if none is stored.
    #[cfg(test)]
    pub async fn get(&self, key: &FeedKey) -> Option<LatestTick> {
        let guard = self.latest.read().await;
        guard.get(key).cloned()
    }

    /// Returns the best bid and the best ask of the latest tick of `key`, or `None` if no tick is stored or the tick lacks either.
    pub async fn latest_bid_ask(&self, key: &FeedKey) -> Option<(Decimal, Decimal)> {
        let guard = self.latest.read().await;
        let tick = guard.get(key)?;
        match (tick.best_bid, tick.best_ask) {
            (Some(bid), Some(ask)) => Some((bid, ask)),
            _ => None,
        }
    }

    /// Returns the age in seconds of the newest latest tick among the feeds of `pair` on any exchange, or `None` if none is stored. An age below zero is returned as zero.
    pub async fn last_tick_age(&self, pair: &Pair) -> Option<u64> {
        let guard = self.latest.read().await;
        let newest_ts = guard
            .iter()
            .filter(|(k, _)| &k.pair == pair)
            .map(|(_, v)| v.ts)
            .max()?;
        let age = (chrono::Utc::now() - newest_ts).num_seconds().max(0) as u64;
        Some(age)
    }

    /// Returns the age in seconds of the latest tick of `key`, or `None` if none is stored. An age below zero is returned as zero.
    pub async fn last_tick_age_for(&self, key: &FeedKey) -> Option<u64> {
        let guard = self.latest.read().await;
        let tick = guard.get(key)?;
        let age = (chrono::Utc::now() - tick.ts).num_seconds().max(0) as u64;
        Some(age)
    }

    /// Returns the mid of the best bid and the best ask of the newest latest tick among the feeds of `pair` on any exchange, or the price of that tick if it lacks either, or `None` if none is stored.
    pub async fn mid(&self, pair: &Pair) -> Option<Decimal> {
        let guard = self.latest.read().await;
        let tick = guard
            .iter()
            .filter(|(k, _)| &k.pair == pair)
            .max_by_key(|(_, v)| v.ts)
            .map(|(_, v)| v)?;
        let mid = match (tick.best_bid, tick.best_ask) {
            (Some(bid), Some(ask)) => (bid + ask) / Decimal::from(2),
            _ => tick.price,
        };
        Some(mid)
    }

    /// Returns a copy of the latest tick of every stored feed.
    pub async fn snapshot(&self) -> Vec<(FeedKey, LatestTick)> {
        let guard = self.latest.read().await;
        guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    async fn read_market_closed_before_latest(
        &self,
    ) -> (
        RwLockReadGuard<'_, HashMap<FeedKey, DateTime<Utc>>>,
        RwLockReadGuard<'_, HashMap<FeedKey, LatestTick>>,
    ) {
        let closed = self.market_closed.read().await;
        let guard = self.latest.read().await;
        (closed, guard)
    }

    /// Returns the health of every expected feed as of `now`. A feed with a closed mark is `MarketClosed`; a feed without a tick is `Missing`; otherwise it is `Healthy` if the age of its tick is within the stale threshold and `Stale` if not. An age below zero is reported as zero.
    pub async fn health_at(&self, now: DateTime<Utc>) -> Vec<FeedHealth> {
        let (closed, guard) = self.read_market_closed_before_latest().await;
        self.expected
            .iter()
            .map(|key| {
                let observed = guard.get(key);
                let market_closed_at = closed.get(key).copied();
                let is_closed = market_closed_at.is_some();
                let (status, age) = match observed {
                    None if is_closed => (FeedStatus::MarketClosed, None),
                    None => (FeedStatus::Missing, None),
                    Some(tick) => {
                        let raw_age = (now - tick.ts).num_seconds();
                        let age_secs = raw_age.max(0);
                        let status = if is_closed {
                            FeedStatus::MarketClosed
                        } else if age_secs <= STALE_THRESHOLD_SECS {
                            FeedStatus::Healthy
                        } else {
                            FeedStatus::Stale
                        };
                        (status, Some(age_secs))
                    }
                };
                FeedHealth {
                    exchange: key.exchange.as_str().to_string(),
                    pair: key.pair.0.clone(),
                    status,
                    last_tick_age_secs: age,
                    market_closed_at,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
