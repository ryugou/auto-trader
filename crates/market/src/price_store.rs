use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::types::{Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use tokio::sync::{RwLock, RwLockReadGuard};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedKey {
    pub exchange: Exchange,
    pub pair: Pair,
}

impl FeedKey {
    pub fn new(exchange: Exchange, pair: Pair) -> Self {
        Self { exchange, pair }
    }
}

#[derive(Debug, Clone)]
pub struct LatestTick {
    pub price: Decimal,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedStatus {
    Healthy,
    Stale,
    Missing,
    MarketClosed,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedHealth {
    pub exchange: String,
    pub pair: String,
    pub status: FeedStatus,
    pub last_tick_age_secs: Option<i64>,
    pub market_closed_at: Option<DateTime<Utc>>,
}

pub const STALE_THRESHOLD_SECS: i64 = 60;

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

    pub async fn update(&self, key: FeedKey, tick: LatestTick) {
        self.clear_market_closed_on_tick(&key).await;
        let mut guard = self.latest.write().await;
        if is_newer_than_stored(guard.get(&key), tick.ts) {
            guard.insert(key, tick);
        }
    }

    pub async fn mark_market_closed(&self, key: FeedKey) {
        self.market_closed.write().await.insert(key, Utc::now());
    }

    #[cfg(test)]
    pub async fn get(&self, key: &FeedKey) -> Option<LatestTick> {
        let guard = self.latest.read().await;
        guard.get(key).cloned()
    }

    pub async fn latest_bid_ask(&self, key: &FeedKey) -> Option<(Decimal, Decimal)> {
        let guard = self.latest.read().await;
        let tick = guard.get(key)?;
        match (tick.best_bid, tick.best_ask) {
            (Some(bid), Some(ask)) => Some((bid, ask)),
            _ => None,
        }
    }

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

    pub async fn last_tick_age_for(&self, key: &FeedKey) -> Option<u64> {
        let guard = self.latest.read().await;
        let tick = guard.get(key)?;
        let age = (chrono::Utc::now() - tick.ts).num_seconds().max(0) as u64;
        Some(age)
    }

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
