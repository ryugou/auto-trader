# Market Feed Health + Positions Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface market-feed stoppages with a prominent dashboard banner, expose latest tick prices via API, add an unrealized P&L column to Positions, and clean up column names + integer display across Positions and Trades tabs.

**Architecture:** A new `PriceStore` (Arc + RwLock<HashMap<(Exchange, Pair), LatestTick>>) lives in `auto-trader-app`, is updated from the existing `price_rx` loop in `main.rs`, and is shared via `AppState`. Two new HTTP handlers expose the snapshot (`/api/market/prices`) and the staleness rollup (`/api/health/market-feed`). The React dashboard adds a `MarketFeedHealthBanner` polled every 15s that renders only when any feed is non-healthy, plus a Positions page upgrade that uses the price snapshot to compute `(current - entry) × quantity`.

**Tech Stack:** Rust (axum, sqlx 0.8, anyhow, chrono, rust_decimal, tokio sync), Postgres (no migration this PR), React 19 + TanStack Query v5 + Tailwind v4. No frontend test framework — verification via `cargo check / clippy / test --workspace` + `npm run lint` + `npm run build` + manual smoke.

**Spec:** `docs/superpowers/specs/2026-04-08-market-feed-health-and-positions-design.md`

---

## File Structure

**New files:**
- `crates/app/src/price_store.rs` — `PriceStore`, `LatestTick`, `FeedKey`, `FeedStatus`, `FeedHealth` types + unit tests
- `crates/app/src/api/market.rs` — `GET /api/market/prices` handler + response DTO
- `crates/app/src/api/health.rs` — `GET /api/health/market-feed` handler + response DTO
- `dashboard-ui/src/components/MarketFeedHealthBanner.tsx` — banner component, polls health and renders only when degraded

**Modified files:**
- `crates/app/src/main.rs` — declare `mod price_store;`, build the expected feed list, construct `Arc<PriceStore>`, pass it to `AppState`, write to it from inside the existing `price_rx` branch
- `crates/app/src/api/mod.rs` — add `pub price_store` field to `AppState`, declare `mod market; mod health;`, register the two new routes
- `dashboard-ui/src/api/types.ts` — add `MarketPrice`, `MarketPricesResponse`, `MarketFeedStatus`, `MarketFeedHealth`, `MarketFeedHealthResponse` types
- `dashboard-ui/src/api/client.ts` — add `api.market.prices()`, `api.health.marketFeed()` plus the new imports
- `dashboard-ui/src/App.tsx` — reorder `navItems` so Positions comes right after Overview; render `<MarketFeedHealthBanner />` between `<header>` and `<main>`
- `dashboard-ui/src/pages/Positions.tsx` — add 含み損益 column, rename SL/TP to 損切りライン/利確ライン, force integer display
- `dashboard-ui/src/components/TradeTable.tsx` — drop the `pnl_amount` column, rename `Net PnL` → `純損益`, integer display for entry/exit/quantity/fees

**Untouched (intentional):**
- `crates/core/src/types.rs` — `Exchange`/`Pair`/`Trade` shapes are reused unchanged
- `crates/executor/src/paper.rs` — `close_position`'s legacy `price_diff × leverage` branch stays as dead code (not on any live path)
- Backend migrations (no DB changes)

---

## Task 1: `PriceStore` module + unit tests

**Files:**
- Create: `crates/app/src/price_store.rs`

- [ ] **Step 1: Create `crates/app/src/price_store.rs`**

```rust
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
```

- [ ] **Step 2: Wire the module into the app crate**

The `auto-trader` binary already has `crates/app/src/main.rs` as its entry. The new `price_store` module will be declared from `main.rs` (Task 2). But to keep this task's commit self-contained and the module compilable on its own, also add a temporary `mod price_store;` line at the top of `main.rs` now (Task 2 will use it).

Edit `crates/app/src/main.rs` and add as the very first non-doc line:

```rust
mod price_store;
```

(immediately above the existing `use auto_trader_core::...` imports.)

- [ ] **Step 3: Run the unit tests**

```bash
source ~/.cargo/env
cargo test -p auto-trader price_store::tests --bin auto-trader
```

Expected: 7 tests passing. If `--bin auto-trader` is wrong (the package may be named differently), fall back to `cargo test -p auto-trader price_store::tests`.

- [ ] **Step 4: Run cargo check and clippy on the whole app crate**

```bash
cargo check -p auto-trader
cargo clippy -p auto-trader -- -D warnings
```

Both must pass.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/price_store.rs crates/app/src/main.rs
git commit -m "feat(app): add PriceStore for tracking latest market ticks"
```

---

## Task 2: Wire `PriceStore` into main.rs and AppState

**Files:**
- Modify: `crates/app/src/main.rs`
- Modify: `crates/app/src/api/mod.rs`

- [ ] **Step 1: Add `price_store` field to `AppState`**

In `crates/app/src/api/mod.rs`, find:

```rust
#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
}
```

Replace with:

```rust
use crate::price_store::PriceStore;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub price_store: Arc<PriceStore>,
}
```

(Add the two `use` lines near the top of the file with the other imports if they aren't already there.)

- [ ] **Step 2: Build the expected feed list and `PriceStore` in main.rs**

In `crates/app/src/main.rs`, after the `let pool = create_pool(...)` line and before the `let (price_tx, mut price_rx) = mpsc::channel::<PriceEvent>(256);` line, add:

```rust
    // Build the expected feed list — (exchange, pair) tuples this
    // process is configured to monitor. The health endpoint will use
    // this list to distinguish "intentionally disabled" from
    // "expected but missing", so OANDA without an API key never
    // shows up as a stale alarm.
    let mut expected_feeds: Vec<crate::price_store::FeedKey> = Vec::new();
```

We'll populate `expected_feeds` in the existing FX-monitor and bitflyer-monitor branches further down. Then immediately after the existing FX `fx_monitor` block (around line 87, just after the trailing `};` of the `let fx_monitor: Option<MarketMonitor> = ...;` assignment), add:

```rust
    if fx_monitor.is_some() {
        for p in &fx_pairs {
            expected_feeds.push(crate::price_store::FeedKey::new(
                auto_trader_core::types::Exchange::Oanda,
                p.clone(),
            ));
        }
    }
```

In the bitflyer block (around line 440-466), inside the `if let Some(bf_config) = &config.bitflyer { if !crypto_pairs.is_empty() { ... } }` body, just before `let mut bf_monitor = ...`, add:

```rust
            for p in &crypto_pairs {
                expected_feeds.push(crate::price_store::FeedKey::new(
                    auto_trader_core::types::Exchange::BitflyerCfd,
                    p.clone(),
                ));
            }
```

Then, after all the monitor setup but before the API server starts (search for `AppState {` in main.rs to find the construction site, or look around line ~1000-1100 area for `let api_router` / `let app_state` / similar), construct the price store:

```rust
    let price_store = auto_trader_app::price_store::PriceStore::new(expected_feeds);
```

If `auto_trader_app::price_store` doesn't resolve (because the binary doesn't expose modules under that path), use `crate::price_store::PriceStore::new(expected_feeds)` instead. The right form depends on whether `price_store` is exposed via `lib.rs` or only from the binary; this codebase uses `main.rs` only, so `crate::price_store::PriceStore::new(expected_feeds)` is correct.

Find where `AppState` is currently constructed (search `AppState {`) and update it to include `price_store`:

```rust
    let app_state = AppState {
        pool: pool.clone(),
        price_store: price_store.clone(),
    };
```

- [ ] **Step 3: Update the price_rx loop to write into PriceStore**

Find the `engine_handle` task in main.rs (around line 656). Inside the `tokio::select!` arm `price = price_rx.recv() => { match price { Some(event) => { ... } } }`, immediately at the top of the `Some(event) =>` block (before the existing `if event.exchange == auto_trader_core::types::Exchange::Oanda` forward), add:

```rust
                            // Snapshot this tick into the in-memory
                            // store so the dashboard health endpoint
                            // can tell whether each configured feed
                            // is fresh.
                            price_store_for_engine.update(
                                crate::price_store::FeedKey::new(
                                    event.exchange,
                                    event.pair.clone(),
                                ),
                                crate::price_store::LatestTick {
                                    price: event.candle.close,
                                    ts: event.timestamp,
                                },
                            ).await;
```

The capture `price_store_for_engine` needs to be cloned before the `tokio::spawn(async move { ... })` for the engine task:

```rust
    let price_store_for_engine = price_store.clone();
    let engine_handle = tokio::spawn(async move {
        let mut macro_rx = macro_rx;
        loop {
            tokio::select! {
                price = price_rx.recv() => {
                    // ...existing body...
                }
                // ...
            }
        }
    });
```

- [ ] **Step 4: cargo check / clippy / test the workspace**

```bash
source ~/.cargo/env
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

All must pass. If you get a "use of moved value: `price_store`" error, the clone happened too late — make sure `let price_store_for_engine = price_store.clone()` runs before any `tokio::spawn` that captures price_store, and that the AppState construction also gets a clone.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/main.rs crates/app/src/api/mod.rs
git commit -m "feat(app): wire PriceStore into price_rx loop and AppState"
```

---

## Task 3: Market and health API handlers

**Files:**
- Create: `crates/app/src/api/market.rs`
- Create: `crates/app/src/api/health.rs`
- Modify: `crates/app/src/api/mod.rs`

- [ ] **Step 1: Create `crates/app/src/api/market.rs`**

```rust
use super::{ApiError, AppState};
use axum::extract::State;
use axum::Json;
use rust_decimal::Decimal;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MarketPrice {
    pub exchange: String,
    pub pair: String,
    pub price: Decimal,
    pub ts: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct MarketPricesResponse {
    pub prices: Vec<MarketPrice>,
}

/// `GET /api/market/prices` — current snapshot of every (exchange,
/// pair) tuple that has at least one observed tick. Used by the
/// Positions page to compute unrealized P&L. Pages and large
/// historical pulls are intentionally not supported here — this
/// endpoint exists to be cheap and frequently polled.
pub async fn prices(
    State(state): State<AppState>,
) -> Result<Json<MarketPricesResponse>, ApiError> {
    let snapshot = state.price_store.snapshot().await;
    let prices = snapshot
        .into_iter()
        .map(|(key, tick)| MarketPrice {
            exchange: key.exchange.as_str().to_string(),
            pair: key.pair.0,
            price: tick.price,
            ts: tick.ts,
        })
        .collect();
    Ok(Json(MarketPricesResponse { prices }))
}
```

- [ ] **Step 2: Create `crates/app/src/api/health.rs`**

```rust
use super::{ApiError, AppState};
use crate::price_store::FeedHealth;
use axum::extract::State;
use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MarketFeedHealthResponse {
    pub feeds: Vec<FeedHealth>,
}

/// `GET /api/health/market-feed` — rollup of every expected feed
/// against its observed last tick. The status is one of:
///   - `healthy`: a tick is present and not older than 60 seconds
///   - `stale`: a tick is present but older than 60 seconds
///   - `missing`: no tick has been received since process start
///
/// Feeds the operator did NOT configure (e.g. OANDA when the API
/// key is unset) are absent from the response, so the dashboard
/// banner does not raise false alarms.
pub async fn market_feed(
    State(state): State<AppState>,
) -> Result<Json<MarketFeedHealthResponse>, ApiError> {
    let feeds = state.price_store.health_at(chrono::Utc::now()).await;
    Ok(Json(MarketFeedHealthResponse { feeds }))
}
```

- [ ] **Step 3: Register routes in `crates/app/src/api/mod.rs`**

Add the module declarations near the existing `mod` lines (alphabetical insert):

```rust
mod accounts;
mod dashboard;
pub(crate) mod filters;
mod health;
mod market;
mod notifications;
mod positions;
mod strategies;
mod trades;
```

In the `router()` function, inside the `api_routes` builder (just before the `.layer(middleware::from_fn(...))` call), add:

```rust
        .route("/market/prices", get(market::prices))
        .route("/health/market-feed", get(health::market_feed))
```

- [ ] **Step 4: Verify**

```bash
source ~/.cargo/env
cargo check -p auto-trader
cargo clippy -p auto-trader -- -D warnings
cargo test -p auto-trader
```

All must pass.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/api/market.rs crates/app/src/api/health.rs crates/app/src/api/mod.rs
git commit -m "feat(api): add /api/market/prices and /api/health/market-feed"
```

---

## Task 4: Frontend types and API client

**Files:**
- Modify: `dashboard-ui/src/api/types.ts`
- Modify: `dashboard-ui/src/api/client.ts`

- [ ] **Step 1: Append market and health types to `dashboard-ui/src/api/types.ts`**

Append at the end of the file:

```ts
export interface MarketPrice {
  exchange: string
  pair: string
  price: string
  ts: string
}

export interface MarketPricesResponse {
  prices: MarketPrice[]
}

export type MarketFeedStatus = 'healthy' | 'stale' | 'missing'

export interface MarketFeedHealth {
  exchange: string
  pair: string
  status: MarketFeedStatus
  last_tick_age_secs: number | null
}

export interface MarketFeedHealthResponse {
  feeds: MarketFeedHealth[]
}
```

- [ ] **Step 2: Add API methods to `dashboard-ui/src/api/client.ts`**

Add `MarketPricesResponse` and `MarketFeedHealthResponse` to the existing `import type { ... } from './types'` block (do not create a second import).

After the existing `notifications` block (the last property of the `api` object), add:

```ts
  market: {
    prices: () => get<MarketPricesResponse>(`/api/market/prices`),
  },
  health: {
    marketFeed: () =>
      get<MarketFeedHealthResponse>(`/api/health/market-feed`),
  },
```

- [ ] **Step 3: Verify**

```bash
cd dashboard-ui && npm run lint && npm run build
```

Pre-existing lint errors in `AccountForm.tsx` / `RiskBadge.tsx` are out of scope. Build must pass. No new errors in `client.ts` / `types.ts`.

- [ ] **Step 4: Commit**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
git add dashboard-ui/src/api/types.ts dashboard-ui/src/api/client.ts
git commit -m "feat(ui): add market price + feed health types and client"
```

---

## Task 5: `MarketFeedHealthBanner` component

**Files:**
- Create: `dashboard-ui/src/components/MarketFeedHealthBanner.tsx`

- [ ] **Step 1: Create the banner component**

```tsx
import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import type { MarketFeedHealth } from '../api/types'

function formatAgeMinutes(secs: number | null): string {
  if (secs == null) return 'tick 未受信'
  if (secs < 60) return `最終 tick ${secs} 秒前`
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `最終 tick ${mins} 分前`
  const hours = Math.floor(mins / 60)
  return `最終 tick ${hours} 時間前`
}

function describe(f: MarketFeedHealth): string {
  const detail = f.status === 'missing' ? 'tick 未受信' : formatAgeMinutes(f.last_tick_age_secs)
  return `${f.exchange} / ${f.pair} (${detail})`
}

export default function MarketFeedHealthBanner() {
  const { data } = useQuery({
    queryKey: ['market-feed-health'],
    queryFn: () => api.health.marketFeed(),
  })

  // While the very first request is in flight we have no data — do
  // not flash a banner. The next 15s tick will give us a real
  // answer; a brief blind window is better than a false-positive
  // alarm on every page load.
  if (!data) return null

  const degraded = data.feeds.filter((f) => f.status !== 'healthy')
  if (degraded.length === 0) return null

  return (
    <div
      role="alert"
      className="bg-red-700 text-white px-4 py-2 text-sm font-semibold border-b border-red-900"
    >
      <div className="max-w-7xl mx-auto flex flex-col gap-1">
        <div className="flex items-center gap-2">
          <span aria-hidden>⚠️</span>
          <span>市場フィード異常</span>
        </div>
        {degraded.map((f) => (
          <div key={`${f.exchange}:${f.pair}`} className="text-xs font-normal pl-6">
            {describe(f)}
          </div>
        ))}
      </div>
    </div>
  )
}
```

- [ ] **Step 2: Lint + build (banner is not yet wired into App, so this only checks the component compiles in isolation)**

```bash
cd dashboard-ui && npm run lint && npm run build
```

Build must pass. Lint must show no new errors.

- [ ] **Step 3: Commit**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
git add dashboard-ui/src/components/MarketFeedHealthBanner.tsx
git commit -m "feat(ui): add MarketFeedHealthBanner component"
```

---

## Task 6: App.tsx — nav reorder + banner placement

**Files:**
- Modify: `dashboard-ui/src/App.tsx`

- [ ] **Step 1: Reorder `navItems` and wire the banner**

Replace the entire `dashboard-ui/src/App.tsx` with:

```tsx
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { BrowserRouter, Routes, Route, NavLink } from 'react-router-dom'
import Overview from './pages/Overview'
import Trades from './pages/Trades'
import Analysis from './pages/Analysis'
import Accounts from './pages/Accounts'
import Positions from './pages/Positions'
import Strategies from './pages/Strategies'
import Notifications from './pages/Notifications'
import NotificationBell from './components/NotificationBell'
import MarketFeedHealthBanner from './components/MarketFeedHealthBanner'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      retry: 1,
      // Auto-refetch every 15 seconds while a query is mounted so the
      // dashboard reflects new positions / fills / balance changes
      // without the user having to hit reload. TanStack defaults to
      // pausing this when the browser tab is in the background, so we
      // don't burn CPU when nobody's watching.
      // (refetchOnWindowFocus is already the TanStack default — left
      // implicit so the config matches the actual behavior.)
      refetchInterval: 15_000,
    },
  },
})

const navItems = [
  { to: '/', label: '概要' },
  { to: '/positions', label: 'ポジション' },
  { to: '/trades', label: 'トレード' },
  { to: '/analysis', label: '分析' },
  { to: '/accounts', label: '口座' },
  { to: '/strategies', label: '戦略' },
]

function NavBar() {
  return (
    <nav className="flex items-center gap-1 overflow-x-auto">
      {navItems.map((item) => (
        <NavLink
          key={item.to}
          to={item.to}
          end={item.to === '/'}
          className={({ isActive }) =>
            `px-3 py-1.5 text-sm rounded transition whitespace-nowrap ${
              isActive
                ? 'bg-gray-800 text-gray-100 font-medium'
                : 'text-gray-400 hover:text-gray-200 hover:bg-gray-800/50'
            }`
          }
        >
          {item.label}
        </NavLink>
      ))}
    </nav>
  )
}

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <div className="min-h-screen bg-gray-950 text-gray-100">
          <header className="border-b border-gray-800 px-4 py-3">
            <div className="max-w-7xl mx-auto flex flex-col sm:flex-row items-start sm:items-center gap-3">
              <h1 className="text-lg font-bold whitespace-nowrap">Auto Trader</h1>
              <NavBar />
              {/* Bell lives flush-right; `ml-auto` inside the component
                  pushes it to the end of the flex row. Deliberately not
                  in `navItems` so it does not render as a tab. */}
              <NotificationBell />
            </div>
          </header>
          <MarketFeedHealthBanner />
          <main className="max-w-7xl mx-auto p-4">
            <Routes>
              <Route path="/" element={<Overview />} />
              <Route path="/trades" element={<Trades />} />
              <Route path="/analysis" element={<Analysis />} />
              <Route path="/accounts" element={<Accounts />} />
              <Route path="/positions" element={<Positions />} />
              <Route path="/strategies" element={<Strategies />} />
              <Route path="/notifications" element={<Notifications />} />
            </Routes>
          </main>
        </div>
      </BrowserRouter>
    </QueryClientProvider>
  )
}

export default App
```

- [ ] **Step 2: Verify**

```bash
cd dashboard-ui && npm run lint && npm run build
```

- [ ] **Step 3: Commit**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
git add dashboard-ui/src/App.tsx
git commit -m "feat(ui): reorder nav (Positions after Overview) + wire feed health banner"
```

---

## Task 7: Positions page — unrealized P&L + column renames + integer display

**Files:**
- Modify: `dashboard-ui/src/pages/Positions.tsx`

- [ ] **Step 1: Replace `dashboard-ui/src/pages/Positions.tsx`**

```tsx
import { useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import PageFilters, { type PageFilterValue } from '../components/PageFilters'
import { RiskBadge, useStrategyRiskLookup } from '../components/RiskBadge'
import type { MarketPrice, PositionResponse } from '../api/types'

function formatInt(value: string | null | undefined): string {
  if (value == null) return '-'
  const n = Number(value)
  if (Number.isNaN(n)) return '-'
  return Math.round(n).toLocaleString()
}

function formatSignedInt(n: number): string {
  if (Number.isNaN(n)) return '-'
  const sign = n > 0 ? '+' : ''
  return `${sign}${Math.round(n).toLocaleString()}`
}

// (current - entry) × quantity for LONG, sign-flipped for SHORT.
// Returns null when we can't compute (no price observed yet, or no
// quantity recorded on the trade row). The banner already alerts on
// missing prices, so we don't try to be clever about the cell here.
function computeUnrealizedPnl(
  position: PositionResponse,
  priceMap: Map<string, MarketPrice>,
): number | null {
  if (position.quantity == null) return null
  const key = `${position.exchange}:${position.pair}`
  const price = priceMap.get(key)
  if (!price) return null
  const current = Number(price.price)
  const entry = Number(position.entry_price)
  const qty = Number(position.quantity)
  if (Number.isNaN(current) || Number.isNaN(entry) || Number.isNaN(qty)) return null
  const diff = position.direction === 'long' ? current - entry : entry - current
  return diff * qty
}

export default function Positions() {
  const queryClient = useQueryClient()
  const [filters, setFilters] = useState<PageFilterValue>({})

  const { data: positions, isLoading } = useQuery({
    queryKey: ['positions'],
    queryFn: () => api.positions.list(),
  })

  const { data: pricesData } = useQuery({
    queryKey: ['market-prices'],
    queryFn: () => api.market.prices(),
  })

  const priceMap = useMemo(() => {
    const m = new Map<string, MarketPrice>()
    pricesData?.prices.forEach((p) => m.set(`${p.exchange}:${p.pair}`, p))
    return m
  }, [pricesData])

  const lookupRisk = useStrategyRiskLookup()

  const handleReload = () => {
    queryClient.invalidateQueries({ queryKey: ['positions'] })
    queryClient.invalidateQueries({ queryKey: ['market-prices'] })
  }

  const JST_OFFSET_MS = 9 * 60 * 60 * 1000
  const toJstDateString = (iso: string) =>
    new Date(new Date(iso).getTime() + JST_OFFSET_MS)
      .toISOString()
      .slice(0, 10)

  const filtered = (positions ?? []).filter((p) => {
    if (filters.exchange && p.exchange !== filters.exchange) return false
    if (
      filters.paper_account_id &&
      p.paper_account_id !== filters.paper_account_id
    )
      return false
    if (filters.from) {
      const entry = toJstDateString(p.entry_at)
      if (entry < filters.from) return false
    }
    if (filters.to) {
      const entry = toJstDateString(p.entry_at)
      if (entry > filters.to) return false
    }
    return true
  })

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold">保有ポジション</h2>
        <button
          onClick={handleReload}
          className="bg-gray-700 hover:bg-gray-600 text-gray-200 text-sm font-medium px-4 py-2 rounded transition"
        >
          リロード
        </button>
      </div>

      <PageFilters value={filters} onChange={setFilters} />

      <div className="bg-gray-900 rounded-lg shadow overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-800">
                <th className="px-4 py-2 text-left text-gray-400 font-medium">戦略</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">ペア</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">取引所</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">方向</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">エントリー価格</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">数量</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">含み損益</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">損切りライン</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">利確ライン</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">エントリー日時</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">口座</th>
              </tr>
            </thead>
            <tbody>
              {isLoading ? (
                <tr>
                  <td colSpan={11} className="px-4 py-8 text-center text-gray-500">
                    読み込み中...
                  </td>
                </tr>
              ) : !filtered.length ? (
                <tr>
                  <td colSpan={11} className="px-4 py-8 text-center text-gray-500">
                    保有ポジションはありません
                  </td>
                </tr>
              ) : (
                filtered.map((p) => {
                  const pnl = computeUnrealizedPnl(p, priceMap)
                  const pnlClass =
                    pnl == null
                      ? 'text-gray-500'
                      : pnl > 0
                        ? 'text-emerald-400'
                        : pnl < 0
                          ? 'text-red-400'
                          : 'text-gray-400'
                  return (
                    <tr
                      key={p.trade_id}
                      className="border-b border-gray-800/50 hover:bg-gray-800/30"
                    >
                      <td className="px-4 py-2">
                        <div className="flex items-center gap-2">
                          <RiskBadge riskLevel={lookupRisk(p.strategy_name)} />
                          <span>{p.strategy_name}</span>
                        </div>
                      </td>
                      <td className="px-4 py-2">{p.pair}</td>
                      <td className="px-4 py-2 text-gray-300">{p.exchange}</td>
                      <td className="px-4 py-2">
                        <span
                          className={
                            p.direction === 'long'
                              ? 'text-emerald-400'
                              : 'text-red-400'
                          }
                        >
                          {p.direction.toUpperCase()}
                        </span>
                      </td>
                      <td className="px-4 py-2 text-right font-mono">
                        {formatInt(p.entry_price)}
                      </td>
                      <td className="px-4 py-2 text-right font-mono">
                        {formatInt(p.quantity)}
                      </td>
                      <td className={`px-4 py-2 text-right font-mono ${pnlClass}`}>
                        {pnl == null ? '-' : formatSignedInt(pnl)}
                      </td>
                      <td className="px-4 py-2 text-right font-mono">
                        {formatInt(p.stop_loss)}
                      </td>
                      <td className="px-4 py-2 text-right font-mono">
                        {formatInt(p.take_profit)}
                      </td>
                      <td className="px-4 py-2 text-gray-300">
                        {new Date(p.entry_at).toLocaleString('ja-JP', {
                          // Pin to JST so the entry time matches what
                          // the trader logs on the server (which is
                          // also JST-scheduled).
                          timeZone: 'Asia/Tokyo',
                          month: '2-digit',
                          day: '2-digit',
                          hour: '2-digit',
                          minute: '2-digit',
                        })}
                      </td>
                      <td className="px-4 py-2 text-gray-300">
                        {p.paper_account_name || '-'}
                      </td>
                    </tr>
                  )
                })
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  )
}
```

- [ ] **Step 2: Verify**

```bash
cd dashboard-ui && npm run lint && npm run build
```

- [ ] **Step 3: Commit**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
git add dashboard-ui/src/pages/Positions.tsx
git commit -m "feat(ui): add 含み損益 column to Positions, rename SL/TP, integer display"
```

---

## Task 8: TradeTable — drop PnL column, rename Net PnL → 純損益, integer display

**Files:**
- Modify: `dashboard-ui/src/components/TradeTable.tsx`

- [ ] **Step 1: Read the current `buildColumns` function**

Open `dashboard-ui/src/components/TradeTable.tsx` and locate the `buildColumns` helper (around line 80-180). You will:

1. Delete the `col.accessor('pnl_amount', { ... })` block entirely
2. Rename the `col.display({ id: 'net_pnl', header: 'Net PnL', ... })` header from `'Net PnL'` to `'純損益'`
3. Add `formatInt` helper at the top of the file (next to `formatNum` / `formatDate`)
4. Replace the body of `entry_price`, `exit_price`, `quantity`, and `fees` cells to use `formatInt(info.getValue())` instead of `formatNum(info.getValue())`

- [ ] **Step 2: Add `formatInt` helper near the top of the file**

Find the existing `formatNum` function (around line 38):

```ts
function formatNum(value: string | null): string {
  if (!value) return '-'
  return Number(value).toLocaleString()
}
```

Add immediately after it:

```ts
function formatInt(value: string | null): string {
  if (!value) return '-'
  const n = Number(value)
  if (Number.isNaN(n)) return '-'
  return Math.round(n).toLocaleString()
}
```

- [ ] **Step 3: Delete the `pnl_amount` column accessor**

Find this block in `buildColumns`:

```tsx
    col.accessor('pnl_amount', {
      header: 'PnL',
      cell: (info) => {
        const val = info.getValue()
        if (!val) return '-'
        const n = Number(val)
        return (
          <span className={n >= 0 ? 'text-emerald-400' : 'text-red-400'}>
            {n >= 0 ? '+' : ''}{Math.round(n).toLocaleString()}
          </span>
        )
      },
    }),
```

Delete it entirely.

- [ ] **Step 4: Rename the `net_pnl` display column header**

Find this block in `buildColumns`:

```tsx
    col.display({
      id: 'net_pnl',
      header: 'Net PnL',
      cell: (info) => {
```

Replace `header: 'Net PnL',` with `header: '純損益',`. Leave the cell function unchanged — it already integer-rounds via `Math.round(net).toLocaleString()`.

- [ ] **Step 5: Switch entry/exit/quantity/fees to `formatInt`**

Find the four lines in `buildColumns`:

```tsx
    col.accessor('entry_price', {
      header: 'エントリー',
      cell: (info) => formatNum(info.getValue()),
    }),
    col.accessor('exit_price', {
      header: 'エグジット',
      cell: (info) => formatNum(info.getValue()),
    }),
    col.accessor('quantity', {
      header: '数量',
      cell: (info) => formatNum(info.getValue()),
    }),
    ...
    col.accessor('fees', {
      header: '手数料',
      cell: (info) => formatNum(info.getValue()),
    }),
```

Replace each `formatNum(info.getValue())` with `formatInt(info.getValue())` (4 occurrences).

- [ ] **Step 6: Verify**

```bash
cd dashboard-ui && npm run lint && npm run build
```

Pre-existing lint errors in unrelated files are out of scope. No new errors. Build must pass.

- [ ] **Step 7: Commit**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
git add dashboard-ui/src/components/TradeTable.tsx
git commit -m "feat(ui): drop PnL column, rename Net PnL → 純損益, integer display"
```

---

## Task 9: Integration verification

**Files:** None (verification only)

- [ ] **Step 1: Full workspace check + clippy + test**

```bash
source ~/.cargo/env
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

All must pass.

- [ ] **Step 2: Frontend lint + build**

```bash
cd dashboard-ui && npm run lint && npm run build
```

The 3 pre-existing lint errors in `AccountForm.tsx` / `RiskBadge.tsx` are allowed. No new errors.

- [ ] **Step 3: Rebuild docker image**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
docker compose build auto-trader
```

Expected: build succeeds.

- [ ] **Step 4: Restart container**

```bash
docker compose up -d auto-trader
sleep 5
docker logs auto-trader-auto-trader-1 --tail 30
```

Expected: container starts cleanly, `API server listening on 0.0.0.0:3001`, `bitflyer websocket connected`, no panics.

- [ ] **Step 5: Smoke test the API endpoints**

```bash
curl -s http://localhost:3001/api/health/market-feed | python3 -m json.tool
curl -s http://localhost:3001/api/market/prices | python3 -m json.tool
```

Expected:
- `/api/health/market-feed` returns `{ "feeds": [{"exchange":"bitflyer_cfd","pair":"FX_BTC_JPY","status":"healthy","last_tick_age_secs": <number>}] }` (assuming bitflyer is configured)
- `/api/market/prices` returns at least one price entry once a tick has arrived (may be empty for the first second after restart)

- [ ] **Step 6: Manual visual smoke test**

Open the dashboard and walk through:

- [ ] ナビ順が `概要 / ポジション / トレード / 分析 / 口座 / 戦略` になっている
- [ ] 健全時はバナーが表示されない
- [ ] ポジションタブに「含み損益」列が追加されている
- [ ] crypto ポジションがあれば、含み損益が数値で出ていて、色付き (緑/赤) になっている
- [ ] ポジションタブの SL/TP が `損切りライン` / `利確ライン` に変わっている
- [ ] ポジションタブの数値カラムが整数表示
- [ ] トレードタブから PnL カラムが消えている
- [ ] トレードタブの `Net PnL` が `純損益` になっている
- [ ] トレードタブの数値カラムが整数表示
- [ ] ベル + 通知 dropdown が壊れていない
- [ ] 開発者ツールの console / network エラー無し

- [ ] **Step 7: Banner failure mode smoke (optional, recommended)**

To verify the banner actually fires, temporarily kill the bitflyer WebSocket connection by stopping the auto-trader container for ~70 seconds:

```bash
docker compose stop auto-trader
sleep 70
# Reload dashboard
# Expected: red banner appears at the top: "市場フィード異常"
docker compose start auto-trader
# Wait ~15s for the banner to clear after the next health poll
```

If you don't want to disturb the running trader, skip Step 7 — Step 5's API check is enough to confirm the wiring.

- [ ] **Step 8: Report findings**

Summarize what passed and anything off-spec. Do not commit or push if smoke tests fail — fix first.

---

## Post-Implementation

1. Run the `code-review` skill flow per project convention: local codex review loop, fix findings, push, open PR with `gh pr create`, request Copilot review (note: API request often fails silently, ask the user to trigger from GitHub Web UI if so), address Copilot findings (1 round per skill rule).
2. Do NOT merge — user does that.
