//! In-memory store of the latest market tick per (exchange, pair).
//!
//! Fed by the `price_rx` loop in `main.rs` (one update per candle
//! tick), read by the `/api/market/prices` and
//! `/api/health/market-feed` handlers via `AppState`.
//!
//! The store also remembers an "expected" feed list — the set of
//! (exchange, pair) tuples the operator configured this process to
//! monitor at startup. The health endpoint walks the expected list,
//! not the observed map, so an intentionally-disabled feed (e.g.
//! OANDA when no API key is set) does not show up as "missing".

use auto_trader_core::types::{Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

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
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedStatus {
    Healthy,
    Stale,
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedHealth {
    pub exchange: String,
    pub pair: String,
    pub status: FeedStatus,
    pub last_tick_age_secs: Option<i64>,
}

/// 60 second window. A tick newer than this counts as healthy.
pub const STALE_THRESHOLD_SECS: i64 = 60;

#[derive(Debug)]
pub struct PriceStore {
    latest: RwLock<HashMap<FeedKey, LatestTick>>,
    expected: Vec<FeedKey>,
}

impl PriceStore {
    pub fn new(expected: Vec<FeedKey>) -> Arc<Self> {
        Arc::new(Self {
            latest: RwLock::new(HashMap::new()),
            expected,
        })
    }

    pub async fn update(&self, key: FeedKey, tick: LatestTick) {
        let mut guard = self.latest.write().await;
        guard.insert(key, tick);
    }

    pub async fn get(&self, key: &FeedKey) -> Option<LatestTick> {
        let guard = self.latest.read().await;
        guard.get(key).cloned()
    }

    pub async fn snapshot(&self) -> Vec<(FeedKey, LatestTick)> {
        let guard = self.latest.read().await;
        guard
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn expected(&self) -> &[FeedKey] {
        &self.expected
    }

    /// Roll the expected list against the current observed map and a
    /// reference "now" timestamp into a vec of `FeedHealth`.
    pub async fn health_at(&self, now: DateTime<Utc>) -> Vec<FeedHealth> {
        let guard = self.latest.read().await;
        self.expected
            .iter()
            .map(|key| {
                let observed = guard.get(key);
                let (status, age) = match observed {
                    None => (FeedStatus::Missing, None),
                    Some(tick) => {
                        let age_secs = (now - tick.ts).num_seconds();
                        let status = if age_secs <= STALE_THRESHOLD_SECS {
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
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal::Decimal;

    fn key(ex: Exchange, p: &str) -> FeedKey {
        FeedKey::new(ex, Pair::new(p))
    }

    fn tick(price: i64, ts: DateTime<Utc>) -> LatestTick {
        LatestTick {
            price: Decimal::from(price),
            ts,
        }
    }

    #[tokio::test]
    async fn update_then_get_returns_latest() {
        let store = PriceStore::new(vec![]);
        let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
        store.update(k.clone(), tick(11_500_000, now)).await;
        let got = store.get(&k).await.expect("present");
        assert_eq!(got.price, Decimal::from(11_500_000));
        assert_eq!(got.ts, now);
    }

    #[tokio::test]
    async fn update_overwrites_previous() {
        let store = PriceStore::new(vec![]);
        let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let t1 = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 30).unwrap();
        store.update(k.clone(), tick(11_500_000, t1)).await;
        store.update(k.clone(), tick(11_600_000, t2)).await;
        let got = store.get(&k).await.unwrap();
        assert_eq!(got.price, Decimal::from(11_600_000));
        assert_eq!(got.ts, t2);
    }

    #[tokio::test]
    async fn health_missing_when_no_tick() {
        let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let store = PriceStore::new(vec![k.clone()]);
        let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
        let report = store.health_at(now).await;
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].status, FeedStatus::Missing);
        assert_eq!(report[0].last_tick_age_secs, None);
    }

    #[tokio::test]
    async fn health_healthy_when_tick_within_60s() {
        let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let store = PriceStore::new(vec![k.clone()]);
        let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
        let recent = now - chrono::Duration::seconds(59);
        store.update(k.clone(), tick(11_500_000, recent)).await;
        let report = store.health_at(now).await;
        assert_eq!(report[0].status, FeedStatus::Healthy);
        assert_eq!(report[0].last_tick_age_secs, Some(59));
    }

    #[tokio::test]
    async fn health_healthy_at_exactly_60s() {
        let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let store = PriceStore::new(vec![k.clone()]);
        let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
        let exactly_60 = now - chrono::Duration::seconds(60);
        store.update(k.clone(), tick(11_500_000, exactly_60)).await;
        let report = store.health_at(now).await;
        assert_eq!(report[0].status, FeedStatus::Healthy);
        assert_eq!(report[0].last_tick_age_secs, Some(60));
    }

    #[tokio::test]
    async fn health_stale_at_61s() {
        let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let store = PriceStore::new(vec![k.clone()]);
        let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
        let old = now - chrono::Duration::seconds(61);
        store.update(k.clone(), tick(11_500_000, old)).await;
        let report = store.health_at(now).await;
        assert_eq!(report[0].status, FeedStatus::Stale);
        assert_eq!(report[0].last_tick_age_secs, Some(61));
    }

    #[tokio::test]
    async fn health_only_reports_expected_feeds() {
        // OANDA is intentionally not in the expected list — even if a
        // tick somehow arrives, the health roll-up should ignore it.
        let bf = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let oanda = key(Exchange::Oanda, "USD_JPY");
        let store = PriceStore::new(vec![bf.clone()]);
        let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
        store.update(oanda.clone(), tick(15_000, now)).await;
        let report = store.health_at(now).await;
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].exchange, "bitflyer_cfd");
    }
}
