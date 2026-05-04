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
use std::collections::{HashMap, HashSet};
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
    /// Last traded price (LTP).
    pub price: Decimal,
    /// Best bid at the time of the tick. `None` for sources without bid/ask
    /// (e.g. OANDA mid-price).
    pub best_bid: Option<Decimal>,
    /// Best ask at the time of the tick. `None` for sources without bid/ask.
    pub best_ask: Option<Decimal>,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedStatus {
    Healthy,
    Stale,
    Missing,
    /// Feed is connected and polling, but the market is closed (weekend/holiday).
    MarketClosed,
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
    /// Feeds where the market is confirmed closed (weekend/holiday).
    /// Distinct from "missing" (never connected) and "stale" (connected but lagging).
    ///
    /// Lock ordering: market_closed first, then latest — same order as update().
    market_closed: RwLock<HashSet<FeedKey>>,
}

impl PriceStore {
    pub fn new(expected: Vec<FeedKey>) -> Arc<Self> {
        Arc::new(Self {
            latest: RwLock::new(HashMap::new()),
            expected,
            market_closed: RwLock::new(HashSet::new()),
        })
    }

    /// Insert a new tick, but only if it is strictly newer than the
    /// last one we already have for this feed. The two callers (raw
    /// websocket tick path and M5 candle path) can interleave with
    /// out-of-order timestamps — bitflyer raw ticks carry real
    /// wall-clock time, while the candle path stamps the period
    /// START. Without this guard, a candle that completes right
    /// after a fresh tick would overwrite the fresh timestamp with
    /// a 5-minute-old one, briefly making the feed look stale.
    pub async fn update(&self, key: FeedKey, tick: LatestTick) {
        // A live tick means the market is open — clear any closed flag.
        self.market_closed.write().await.remove(&key);
        let mut guard = self.latest.write().await;
        match guard.get(&key) {
            Some(existing) if existing.ts >= tick.ts => {
                // Equal or older — drop the incoming write so the
                // store always reflects the newest known tick.
            }
            _ => {
                guard.insert(key, tick);
            }
        }
    }

    /// Mark a feed as having its market closed (weekend/holiday).
    /// The feed is connected and polling, but no ticks will arrive until
    /// the market reopens. Cleared automatically when `update` receives a tick.
    pub async fn mark_market_closed(&self, key: FeedKey) {
        self.market_closed.write().await.insert(key);
    }

    #[cfg(test)]
    pub async fn get(&self, key: &FeedKey) -> Option<LatestTick> {
        let guard = self.latest.read().await;
        guard.get(key).cloned()
    }

    /// Return `(bid, ask)` for the given pair only when both sides are present.
    /// Returns `None` if the feed has never been observed, or if the stored
    /// tick does not carry bid/ask (e.g. OANDA mid-price data).
    pub async fn latest_bid_ask(&self, key: &FeedKey) -> Option<(Decimal, Decimal)> {
        let guard = self.latest.read().await;
        let tick = guard.get(key)?;
        match (tick.best_bid, tick.best_ask) {
            (Some(bid), Some(ask)) => Some((bid, ask)),
            _ => None,
        }
    }

    /// Return the age (in seconds) of the newest tick for the given pair across
    /// all exchanges. Returns `None` when no tick has ever been observed for
    /// that pair.
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

    /// Return the age (in seconds) of the tick for a specific (exchange, pair).
    /// Unlike `last_tick_age` this does NOT search across all exchanges, so it
    /// cannot be contaminated by a tick from a different exchange that happens
    /// to share the same pair name.
    pub async fn last_tick_age_for(&self, key: &FeedKey) -> Option<u64> {
        let guard = self.latest.read().await;
        let tick = guard.get(key)?;
        let age = (chrono::Utc::now() - tick.ts).num_seconds().max(0) as u64;
        Some(age)
    }

    /// Return the mid price for the given pair from the most recent tick that
    /// carries both bid and ask. Falls back to `price` (LTP) when bid/ask are
    /// absent. Returns `None` when no tick has been observed for the pair.
    pub async fn mid(&self, pair: &Pair) -> Option<Decimal> {
        let guard = self.latest.read().await;
        // Pick the newest tick across all exchanges for this pair.
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

    /// Roll the expected list against the current observed map and a
    /// reference "now" timestamp into a vec of `FeedHealth`.
    pub async fn health_at(&self, now: DateTime<Utc>) -> Vec<FeedHealth> {
        // Acquire in same order as update(): market_closed first, then latest.
        let closed = self.market_closed.read().await;
        let guard = self.latest.read().await;
        self.expected
            .iter()
            .map(|key| {
                let observed = guard.get(key);
                let is_closed = closed.contains(key);
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
            best_bid: None,
            best_ask: None,
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
    async fn update_drops_older_or_equal_timestamp() {
        // The two writers (raw tick path and M5 candle path) can
        // interleave with out-of-order timestamps. The store must
        // always reflect the newest known tick, so an older or
        // equal incoming write must be ignored.
        let store = PriceStore::new(vec![]);
        let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let newer = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 30).unwrap();
        let older = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
        store.update(k.clone(), tick(11_600_000, newer)).await;
        // Older write should be a no-op.
        store.update(k.clone(), tick(11_500_000, older)).await;
        let got = store.get(&k).await.unwrap();
        assert_eq!(got.price, Decimal::from(11_600_000));
        assert_eq!(got.ts, newer);
        // Equal timestamp also drops (so we don't churn for an
        // exact-duplicate write).
        store.update(k.clone(), tick(11_700_000, newer)).await;
        let got = store.get(&k).await.unwrap();
        assert_eq!(got.price, Decimal::from(11_600_000));
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
    async fn health_clamps_future_timestamp_to_zero() {
        // An upstream clock-skewed feed that reports a timestamp in
        // the future must not show up as "-5s old" — we clamp to
        // zero and keep reporting healthy (the real problem is
        // upstream, not this feed going stale).
        let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let store = PriceStore::new(vec![k.clone()]);
        let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
        let future = now + chrono::Duration::seconds(5);
        store.update(k.clone(), tick(11_500_000, future)).await;
        let report = store.health_at(now).await;
        assert_eq!(report[0].status, FeedStatus::Healthy);
        assert_eq!(report[0].last_tick_age_secs, Some(0));
    }

    #[tokio::test]
    async fn last_tick_age_returns_small_for_just_inserted_tick() {
        let store = PriceStore::new(vec![]);
        let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let now = chrono::Utc::now();
        store.update(k.clone(), tick(11_500_000, now)).await;
        let age = store
            .last_tick_age(&k.pair)
            .await
            .expect("tick was just inserted");
        assert!(age <= 1, "age should be 0 or 1 second, got {age}");
    }

    #[tokio::test]
    async fn last_tick_age_returns_none_for_unknown_pair() {
        let store = PriceStore::new(vec![]);
        let pair = Pair::new("UNKNOWN_PAIR");
        let age = store.last_tick_age(&pair).await;
        assert!(age.is_none(), "expected None for unknown pair");
    }

    #[tokio::test]
    async fn last_tick_age_for_is_exchange_specific() {
        // Two exchanges with the same pair name — last_tick_age_for must
        // return the age for the requested exchange only, not cross-exchange.
        let store = PriceStore::new(vec![]);
        let bf = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
        let gmo = key(Exchange::GmoFx, "FX_BTC_JPY");
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::seconds(120);
        store.update(bf.clone(), tick(11_500_000, now)).await;
        store.update(gmo.clone(), tick(11_500_000, old)).await;

        let bf_age = store.last_tick_age_for(&bf).await.expect("bf present");
        let gmo_age = store.last_tick_age_for(&gmo).await.expect("gmo present");

        // BitflyerCfd should be fresher than GmoFx, and GmoFx should remain
        // substantially older, proving the lookup is exchange-specific and
        // not contaminated by the other exchange's tick.
        assert!(bf_age < gmo_age, "bf age {bf_age} should be less than gmo age {gmo_age}");
        assert!(
            gmo_age >= bf_age + 100,
            "gmo age {gmo_age} should be at least 100s older than bf age {bf_age}"
        );
    }

    #[tokio::test]
    async fn last_tick_age_for_returns_none_for_missing_key() {
        let store = PriceStore::new(vec![]);
        let k = key(Exchange::GmoFx, "USD_JPY");
        let age = store.last_tick_age_for(&k).await;
        assert!(age.is_none());
    }

    #[tokio::test]
    async fn health_market_closed_when_marked() {
        let k = key(Exchange::GmoFx, "USD_JPY");
        let store = PriceStore::new(vec![k.clone()]);
        let now = Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();

        // Initially missing.
        let report = store.health_at(now).await;
        assert_eq!(report[0].status, FeedStatus::Missing);

        // Mark market closed.
        store.mark_market_closed(k.clone()).await;
        let report = store.health_at(now).await;
        assert_eq!(report[0].status, FeedStatus::MarketClosed);
    }

    #[tokio::test]
    async fn health_market_closed_cleared_on_tick() {
        let k = key(Exchange::GmoFx, "USD_JPY");
        let store = PriceStore::new(vec![k.clone()]);
        let now = Utc.with_ymd_and_hms(2026, 5, 4, 9, 0, 0).unwrap();

        // Mark closed, then receive a tick (market reopened).
        store.mark_market_closed(k.clone()).await;
        store.update(k.clone(), tick(160_000, now)).await;
        let report = store.health_at(now).await;
        assert_eq!(report[0].status, FeedStatus::Healthy);
    }

    #[tokio::test]
    async fn health_market_closed_with_stale_tick() {
        // Had a tick before market close, now marked closed.
        let k = key(Exchange::GmoFx, "USD_JPY");
        let store = PriceStore::new(vec![k.clone()]);
        let old = Utc.with_ymd_and_hms(2026, 5, 1, 20, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();

        store.update(k.clone(), tick(160_000, old)).await;
        store.mark_market_closed(k.clone()).await;
        let report = store.health_at(now).await;
        // Should be MarketClosed, not Stale.
        assert_eq!(report[0].status, FeedStatus::MarketClosed);
        // But last_tick_age_secs should still reflect the old tick.
        assert!(report[0].last_tick_age_secs.is_some());
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
