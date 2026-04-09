# FX Paper Trading Enablement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable USD/JPY paper trading on OANDA demo with a new `donchian_trend_fx` strategy running on 4 paper accounts (2 balance tiers × 2 allocation levels), wired into the existing feed-health banner via OANDA pricing stream.

**Architecture:** Mirror the bitflyer pattern: a new `OandaClient::stream_prices` method opens the `/v3/accounts/{id}/pricing/stream` NDJSON endpoint and pushes each `PRICE` (and `HEARTBEAT`-rebroadcast) into the existing `PriceStore` via a new `MarketMonitor::with_raw_tick_sink` builder. A new pair-agnostic `DonchianTrendFxV1` struct is registered twice with different `allocation_pct` constructor args (0.50 and 0.80) so four paper accounts at two balance tiers can run the same logic at two risk levels. The FX position monitor is revived with an `Exchange::Oanda` filter mirroring the crypto monitor.

**Tech Stack:** Rust (axum, sqlx 0.8, reqwest, tokio, chrono, rust_decimal, anyhow), Postgres (1 migration), OANDA v3 REST + streaming. No frontend changes. Verification via `cargo check/clippy/test --workspace` + manual smoke against the running docker container.

**Spec:** `docs/superpowers/specs/2026-04-09-fx-paper-trading-design.md`

---

## File Structure

**New files:**
- `crates/strategy/src/donchian_trend_fx.rs` — pair-agnostic FX Donchian strategy (ATR×2 SL, 10-bar trailing exit, allocation_pct constructor arg) + unit tests
- `migrations/20260409000001_fx_paper_accounts_seed.sql` — 2 strategy catalog rows + 4 paper accounts

**Modified files:**
- `crates/market/src/lib.rs` — pub type `RawTick` moved here from bitflyer
- `crates/market/src/bitflyer.rs` — delete local `pub type RawTick` definition, use `crate::RawTick`
- `crates/market/src/oanda.rs` — add `stream_prices(&self, instruments, tx)` method with HEARTBEAT rebroadcast
- `crates/market/src/monitor.rs` — add `raw_tick_tx` field and `with_raw_tick_sink` builder; spawn a background stream task when the sink is set
- `crates/strategy/src/lib.rs` — `pub mod donchian_trend_fx;`
- `crates/app/src/main.rs` — rename existing `raw_tick_tx` to `bf_raw_tick_tx`, add `oanda_raw_tick_tx`, spawn an OANDA drain task, wire `with_raw_tick_sink` into `fx_monitor`, replace the drain-only `pos_monitor_handle` with a real FX position monitor, register `donchian_trend_fx_*` strategies
- `config/default.toml` — scope `pairs.fx` to `USD_JPY` only (drop `EUR_USD`), add two `[[strategies]]` entries for `donchian_trend_fx_normal` / `donchian_trend_fx_aggressive`

**Untouched:**
- `crates/core/src/types.rs` — Exchange/Pair/Signal all reused
- `crates/executor/src/paper.rs` — `PaperTrader` is already pair/exchange-agnostic
- `crates/db/src/*` — no schema changes beyond the seed
- `crates/app/src/api/*` — DTOs and routes all reused
- `dashboard-ui/*` — frontend already handles multi-exchange display

---

## Task 1: Move `RawTick` to the market crate root

Small refactor so `oanda.rs` can reuse the same type without circular imports.

**Files:**
- Modify: `crates/market/src/lib.rs`
- Modify: `crates/market/src/bitflyer.rs`

- [ ] **Step 1: Add `RawTick` to `crates/market/src/lib.rs`**

Replace the file contents with:

```rust
use auto_trader_core::types::Pair;
use rust_decimal::Decimal;

pub mod bitflyer;
pub mod candle_builder;
pub mod indicators;
pub mod monitor;
pub mod oanda;
pub mod provider;

/// One raw tick observed on any exchange. Used by the dashboard
/// feed-health / PriceStore path (distinct from the candle-aggregated
/// `PriceEvent` channel that strategies consume). Defined here so
/// bitflyer and oanda can both reference it without a circular
/// dependency between the two modules.
pub type RawTick = (Pair, Decimal, chrono::DateTime<chrono::Utc>);
```

- [ ] **Step 2: Remove the local `RawTick` definition from `crates/market/src/bitflyer.rs`**

Find these lines (around `bitflyer.rs:36-42`):

```rust
/// One raw tick observed on the websocket. Sent (best-effort) to a
/// subscriber that wants every price update — typically the
/// dashboard `PriceStore` for freshness monitoring. Distinct from
/// the M5-aggregated `PriceEvent` channel, which only fires on
/// candle boundaries and is therefore unsuitable for "is the feed
/// alive right now?" health checks.
pub type RawTick = (Pair, Decimal, chrono::DateTime<chrono::Utc>);
```

Delete the `pub type RawTick = ...;` line (the comment block can stay if desired — but since the type moved, it's clearer to delete the comment too and leave one canonical doc in `lib.rs`). Delete both the comment and the `pub type` line.

Immediately above the `pub struct BitflyerMonitor`, add:

```rust
use crate::RawTick;
```

(If there is already a `use crate::...` line, add `RawTick` to it.)

- [ ] **Step 3: Compile check**

Run:

```bash
source ~/.cargo/env
cargo check -p auto-trader-market
```

Expected: PASS. If the compiler complains about `RawTick` being undefined in `bitflyer.rs`, the `use crate::RawTick;` line is missing or misplaced.

- [ ] **Step 4: Commit**

```bash
git add crates/market/src/lib.rs crates/market/src/bitflyer.rs
git commit -m "refactor(market): move RawTick to crate root for reuse across providers"
```

---

## Task 2: Implement `OandaClient::stream_prices`

Streaming NDJSON endpoint. Parses `PRICE` events and rebroadcasts the last `PRICE` on `HEARTBEAT` so the feed-health 60s threshold stays green during low-liquidity periods.

**Files:**
- Modify: `crates/market/src/oanda.rs`

- [ ] **Step 1: Add imports at the top of `crates/market/src/oanda.rs`**

Near the existing `use` block, add:

```rust
use crate::RawTick;
use tokio::sync::mpsc;
```

- [ ] **Step 2: Add the `stream_prices` method on `impl OandaClient`**

Append this method at the end of the existing `impl OandaClient { ... }` block (after `get_latest_price` at line 182):

```rust
    /// Open the OANDA pricing stream for the given instruments and
    /// forward every tick into `tx`. The endpoint is NDJSON: one
    /// JSON object per line, typed either `"PRICE"` (a real tick)
    /// or `"HEARTBEAT"` (a 5-second keep-alive with no price).
    ///
    /// We handle `HEARTBEAT` by re-sending the last observed `PRICE`
    /// with the heartbeat's own timestamp. This keeps the
    /// dashboard's feed-health 60-second threshold green during
    /// quiet periods (e.g. the hour after NY close on Friday) when
    /// the price legitimately isn't moving.
    ///
    /// Returns `Err` on connection / parse / send failures; the
    /// caller is expected to reconnect with backoff.
    pub async fn stream_prices(
        &self,
        instruments: &[Pair],
        tx: mpsc::Sender<RawTick>,
    ) -> anyhow::Result<()> {
        use futures_util::StreamExt;

        if instruments.is_empty() {
            return Ok(());
        }
        let instrument_list = instruments
            .iter()
            .map(|p| p.0.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let url = format!(
            "{}/v3/accounts/{}/pricing/stream",
            self.base_url, self.account_id
        );

        let resp = self
            .client
            .get(&url)
            .query(&[("instruments", instrument_list.as_str())])
            .send()
            .await?
            .error_for_status()?;

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        // Cache of the last real PRICE per instrument, used to
        // rebroadcast on HEARTBEAT so the freshness watchdog stays
        // green during quiet periods.
        let mut last_price: std::collections::HashMap<String, Decimal> =
            std::collections::HashMap::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buf.extend_from_slice(&chunk);
            // Split on newlines — the stream is NDJSON.
            while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                let line_str = std::str::from_utf8(&line)?.trim();
                if line_str.is_empty() {
                    continue;
                }
                let msg: serde_json::Value = match serde_json::from_str(line_str) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "OANDA pricing stream: failed to parse line: {e}"
                        );
                        continue;
                    }
                };
                let msg_type = msg["type"].as_str().unwrap_or("");
                let time_str = msg["time"].as_str().unwrap_or("");
                let ts = match chrono::DateTime::parse_from_rfc3339(time_str) {
                    Ok(t) => t.with_timezone(&chrono::Utc),
                    Err(_) => continue,
                };
                match msg_type {
                    "PRICE" => {
                        let instrument = match msg["instrument"].as_str() {
                            Some(s) => s.to_string(),
                            None => continue,
                        };
                        let bid = msg["bids"]
                            .as_array()
                            .and_then(|a| a.first())
                            .and_then(|b| b["price"].as_str())
                            .and_then(|s| Decimal::from_str(s).ok());
                        let ask = msg["asks"]
                            .as_array()
                            .and_then(|a| a.first())
                            .and_then(|a| a["price"].as_str())
                            .and_then(|s| Decimal::from_str(s).ok());
                        let (Some(bid), Some(ask)) = (bid, ask) else {
                            continue;
                        };
                        let mid = (bid + ask) / Decimal::from(2);
                        last_price.insert(instrument.clone(), mid);
                        let pair = Pair::new(&instrument);
                        match tx.try_send((pair, mid, ts)) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                tracing::debug!(
                                    "OANDA raw tick sink full, dropping tick"
                                );
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                tracing::warn!("OANDA raw tick sink closed");
                                return Ok(());
                            }
                        }
                    }
                    "HEARTBEAT" => {
                        // Rebroadcast the most recent real PRICE on
                        // every instrument we have seen, with the
                        // heartbeat's own timestamp. Skip instruments
                        // whose first PRICE has not arrived yet.
                        for (instrument, price) in &last_price {
                            let pair = Pair::new(instrument);
                            match tx.try_send((pair, *price, ts)) {
                                Ok(()) => {}
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    tracing::debug!(
                                        "OANDA raw tick sink full on heartbeat rebroadcast"
                                    );
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    tracing::warn!(
                                        "OANDA raw tick sink closed on heartbeat"
                                    );
                                    return Ok(());
                                }
                            }
                        }
                    }
                    _ => {
                        // Unknown message type — ignore.
                    }
                }
            }
        }
        Ok(())
    }
```

- [ ] **Step 3: Add `futures-util` to the market crate Cargo.toml if it isn't already there**

Check with:

```bash
grep "futures-util" crates/market/Cargo.toml
```

If the line is missing, add to `[dependencies]`:

```toml
futures-util = { workspace = true }
```

Then check the workspace root `Cargo.toml` has `futures-util` under `[workspace.dependencies]`. If not, add:

```toml
futures-util = "0.3"
```

- [ ] **Step 4: cargo check**

```bash
source ~/.cargo/env
cargo check -p auto-trader-market
```

Expected: PASS. If `futures_util::StreamExt` is not resolving, the dependency step failed — re-check `Cargo.toml`.

- [ ] **Step 5: clippy**

```bash
cargo clippy -p auto-trader-market -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/market/src/oanda.rs crates/market/Cargo.toml Cargo.toml
git commit -m "feat(market): OandaClient::stream_prices with HEARTBEAT rebroadcast"
```

(Only include `Cargo.toml` changes in the commit if you actually had to add `futures-util`; otherwise drop from the add.)

---

## Task 3: `MarketMonitor::with_raw_tick_sink`

Add the builder method and spawn a background stream task when the sink is set.

**Files:**
- Modify: `crates/market/src/monitor.rs`

- [ ] **Step 1: Replace `crates/market/src/monitor.rs` with:**

```rust
use crate::oanda::OandaClient;
use crate::{indicators, RawTick};
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

pub struct MarketMonitor {
    client: Arc<OandaClient>,
    pairs: Vec<Pair>,
    interval_secs: u64,
    timeframe: String,
    tx: mpsc::Sender<PriceEvent>,
    pool: Option<PgPool>,
    raw_tick_tx: Option<mpsc::Sender<RawTick>>,
}

impl MarketMonitor {
    pub fn new(
        client: OandaClient,
        pairs: Vec<Pair>,
        interval_secs: u64,
        timeframe: &str,
        tx: mpsc::Sender<PriceEvent>,
    ) -> Self {
        Self {
            client: Arc::new(client),
            pairs,
            interval_secs,
            timeframe: timeframe.to_string(),
            tx,
            pool: None,
            raw_tick_tx: None,
        }
    }

    pub fn with_db(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Subscribe to the OANDA pricing stream so raw ticks flow into
    /// the dashboard's `PriceStore` for feed-health monitoring,
    /// independent of the M-series candle cadence the strategy
    /// engine consumes. See `OandaClient::stream_prices` for the
    /// NDJSON + HEARTBEAT handling details.
    pub fn with_raw_tick_sink(mut self, tx: mpsc::Sender<RawTick>) -> Self {
        self.raw_tick_tx = Some(tx);
        self
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        // Background raw-tick streamer. Runs for the lifetime of
        // this monitor, reconnecting with a 5-second backoff on
        // error. Only spawned when a sink is actually configured.
        if let Some(tx) = self.raw_tick_tx.clone() {
            let client = self.client.clone();
            let pairs = self.pairs.clone();
            tokio::spawn(async move {
                loop {
                    if let Err(e) = client.stream_prices(&pairs, tx.clone()).await {
                        tracing::warn!(
                            "OANDA price stream error (reconnecting in 5s): {e}"
                        );
                    } else {
                        tracing::info!(
                            "OANDA price stream ended cleanly (reconnecting in 5s)"
                        );
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            });
        }

        let mut tick = interval(Duration::from_secs(self.interval_secs));
        loop {
            tick.tick().await;
            for pair in &self.pairs {
                match self.fetch_and_emit(pair).await {
                    Ok(()) => {}
                    Err(e) => {
                        if self.tx.is_closed() {
                            tracing::info!("price channel closed, stopping monitor");
                            return Ok(());
                        }
                        tracing::error!("monitor error for {pair}: {e}");
                    }
                }
            }
        }
    }

    async fn fetch_and_emit(&self, pair: &Pair) -> anyhow::Result<()> {
        let candles = self.client.get_candles(pair, &self.timeframe, 100).await?;
        let latest = match candles.last() {
            Some(c) => c.clone(),
            None => return Ok(()),  // no complete candles
        };

        // Save candles to DB for backtest data accumulation
        if let Some(pool) = &self.pool {
            for candle in &candles {
                if let Err(e) = auto_trader_db::candles::upsert_candle(pool, candle).await {
                    tracing::warn!("failed to save candle: {e}");
                }
            }
        }
        let closes: Vec<Decimal> = candles.iter().map(|c| c.close).collect();

        let mut indicators = HashMap::new();
        if let Some(v) = indicators::sma(&closes, 20) {
            indicators.insert("sma_20".to_string(), v);
        }
        if let Some(v) = indicators::sma(&closes, 50) {
            indicators.insert("sma_50".to_string(), v);
        }
        if let Some(v) = indicators::ema(&closes, 20) {
            indicators.insert("ema_20".to_string(), v);
        }
        if let Some(v) = indicators::rsi(&closes, 14) {
            indicators.insert("rsi_14".to_string(), v);
        }

        let event = PriceEvent {
            pair: pair.clone(),
            exchange: Exchange::Oanda,
            timestamp: latest.timestamp,
            candle: latest,
            indicators,
        };
        self.tx.send(event).await?;
        Ok(())
    }
}
```

Key changes versus the original file:

- `client: OandaClient` → `client: Arc<OandaClient>` so the stream task can clone and own it
- New `raw_tick_tx: Option<mpsc::Sender<RawTick>>` field
- New `with_raw_tick_sink` builder method
- `run()` now spawns a background streamer before the polling loop

- [ ] **Step 2: cargo check + clippy**

```bash
source ~/.cargo/env
cargo check -p auto-trader-market
cargo clippy -p auto-trader-market -- -D warnings
```

Both must pass. If `OandaClient` doesn't implement `Clone`, that's expected — `Arc<OandaClient>` does not require `OandaClient: Clone`.

- [ ] **Step 3: Commit**

```bash
git add crates/market/src/monitor.rs
git commit -m "feat(market): MarketMonitor::with_raw_tick_sink + background streamer"
```

---

## Task 4: `DonchianTrendFxV1` strategy

Pair-agnostic FX Donchian with ATR×2 SL + 10-bar trailing exit + `allocation_pct` constructor arg.

**Files:**
- Create: `crates/strategy/src/donchian_trend_fx.rs`
- Modify: `crates/strategy/src/lib.rs`

- [ ] **Step 1: Append the module declaration to `crates/strategy/src/lib.rs`**

Open the file and add `pub mod donchian_trend_fx;` in alphabetical order next to the other module declarations. For example, if the file currently reads:

```rust
pub mod bb_mean_revert;
pub mod donchian_trend;
pub mod engine;
pub mod squeeze_momentum;
pub mod swing_llm;
```

Change it to:

```rust
pub mod bb_mean_revert;
pub mod donchian_trend;
pub mod donchian_trend_fx;
pub mod engine;
pub mod squeeze_momentum;
pub mod swing_llm;
```

- [ ] **Step 2: Create `crates/strategy/src/donchian_trend_fx.rs`**

```rust
//! FX 標準ブレイクアウト v1 (`donchian_trend_fx_*`).
//!
//! Pair-agnostic port of `donchian_trend_v1` tuned for OANDA FX
//! instruments on M15. Two differences from the crypto version:
//!
//! 1. **ATR-based stop loss** (`entry ± ATR × 2`, Turtle "N" stop)
//!    instead of a flat 3% distance. FX pip volatility at M15 is
//!    too small for a percentage SL to be meaningful.
//! 2. **`allocation_pct` is a constructor argument**, not a
//!    compile-time constant. The strategy is registered twice in
//!    `main.rs` (`donchian_trend_fx_normal` at 0.50 and
//!    `donchian_trend_fx_aggressive` at 0.80) so four paper
//!    accounts at two balance tiers × two risk levels can share
//!    the same code.
//!
//! ## Entry rules
//! - **Long**: current close > prior 20-bar high AND ATR(14) >
//!   rolling average ATR over the prior 50 bars.
//! - **Short**: mirror — current close < 20-bar low AND elevated
//!   ATR.
//!
//! ## Stop loss
//! `entry ± ATR(14) × 2`. Dynamic; adapts to the current volatility
//! regime so the SL is neither trivially tight in calm sessions
//! nor blown through in news events.
//!
//! ## Take profit (dynamic, via `on_open_positions`)
//! - **Long** closes when current close < prior 10-bar low.
//! - **Short** closes when current close > prior 10-bar high.
//!
//! No fixed TP — the trailing channel exit is the strategy's edge.
//!
//! ## Max hold
//! 72 hours from entry. FX trends unfold over days at M15, so
//! `max_hold_until` is set further out than the crypto version.

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{ExitSignal, MacroUpdate, Strategy, StrategyExitReason};
use auto_trader_core::types::{Candle, Direction, Exchange, Pair, Position, Signal};
use auto_trader_market::indicators;
use chrono::Duration;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, VecDeque};

const ENTRY_CHANNEL: usize = 20;
const EXIT_CHANNEL: usize = 10;
const ATR_PERIOD: usize = 14;
const ATR_BASELINE_BARS: usize = 50;
const ATR_SL_MULT: Decimal = dec!(2.0);
const HISTORY_LEN: usize = 200;
const TIME_LIMIT_HOURS: i64 = 72;

pub struct DonchianTrendFxV1 {
    name: String,
    pairs: Vec<Pair>,
    allocation_pct: Decimal,
    history: HashMap<String, VecDeque<Candle>>,
}

impl DonchianTrendFxV1 {
    pub fn new(name: String, pairs: Vec<Pair>, allocation_pct: Decimal) -> Self {
        Self {
            name,
            pairs,
            allocation_pct,
            history: HashMap::new(),
        }
    }

    fn push_candle(&mut self, pair: &str, candle: Candle) {
        let h = self.history.entry(pair.to_string()).or_default();
        h.push_back(candle);
        while h.len() > HISTORY_LEN {
            h.pop_front();
        }
    }

    fn highs(history: &VecDeque<Candle>) -> Vec<Decimal> {
        history.iter().map(|c| c.high).collect()
    }

    fn lows(history: &VecDeque<Candle>) -> Vec<Decimal> {
        history.iter().map(|c| c.low).collect()
    }

    fn closes(history: &VecDeque<Candle>) -> Vec<Decimal> {
        history.iter().map(|c| c.close).collect()
    }

    /// Average ATR over the `ATR_BASELINE_BARS` bars prior to the
    /// current bar (current bar excluded so the breakout-day
    /// volatility doesn't pollute its own baseline). Identical to
    /// the crypto-side helper — kept local to avoid a cross-crate
    /// helper dependency that would require larger refactoring.
    fn baseline_atr(history: &VecDeque<Candle>) -> Option<Decimal> {
        if history.len() < ATR_BASELINE_BARS + ATR_PERIOD + 2 {
            return None;
        }
        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let closes = Self::closes(history);
        let latest_prior = history.len() - 2;
        let start = latest_prior + 1 - ATR_BASELINE_BARS;
        let mut sum = Decimal::ZERO;
        let mut count = 0u32;
        for end in start..=latest_prior {
            if end < ATR_PERIOD + 1 {
                continue;
            }
            if let Some(v) = indicators::atr(
                &highs[..=end],
                &lows[..=end],
                &closes[..=end],
                ATR_PERIOD,
            ) {
                sum += v;
                count += 1;
            }
        }
        if count == 0 {
            return None;
        }
        Some(sum / Decimal::from(count))
    }
}

#[async_trait::async_trait]
impl Strategy for DonchianTrendFxV1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if event.exchange != Exchange::Oanda {
            return None;
        }
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }
        let key = event.pair.0.clone();
        self.push_candle(&key, event.candle.clone());
        let history = self.history.get(&key)?;

        // Need enough history for the entry channel + ATR + baseline.
        if history.len() < ENTRY_CHANNEL + ATR_BASELINE_BARS + ATR_PERIOD + 1 {
            return None;
        }

        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let closes = Self::closes(history);

        // Entry channel uses prior bars only (current excluded).
        let (channel_low, channel_high) =
            indicators::donchian_channel(&highs, &lows, ENTRY_CHANNEL, false)?;

        let atr = indicators::atr(&highs, &lows, &closes, ATR_PERIOD)?;
        let baseline = Self::baseline_atr(history)?;
        if atr <= baseline {
            // Volatility too tame — likely a false breakout.
            return None;
        }

        let entry = event.candle.close;
        let sl_offset = atr * ATR_SL_MULT;
        let max_hold = Some(event.timestamp + Duration::hours(TIME_LIMIT_HOURS));

        if entry > channel_high {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                entry_price: entry,
                stop_loss: entry - sl_offset,
                // Fixed TP parked far away — the real exit is the
                // trailing 10-bar Donchian in `on_open_positions`.
                take_profit: entry * dec!(1000),
                confidence: 0.6,
                timestamp: event.timestamp,
                allocation_pct: self.allocation_pct,
                max_hold_until: max_hold,
            });
        }
        if entry < channel_low {
            return Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                entry_price: entry,
                stop_loss: entry + sl_offset,
                take_profit: entry / dec!(1000),
                confidence: 0.6,
                timestamp: event.timestamp,
                allocation_pct: self.allocation_pct,
                max_hold_until: max_hold,
            });
        }
        None
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {}

    async fn warmup(&mut self, events: &[PriceEvent]) {
        for event in events {
            if event.exchange != Exchange::Oanda {
                continue;
            }
            if !self.pairs.iter().any(|p| p == &event.pair) {
                continue;
            }
            self.push_candle(&event.pair.0, event.candle.clone());
        }
    }

    async fn on_open_positions(
        &mut self,
        positions: &[Position],
        event: &PriceEvent,
    ) -> Vec<ExitSignal> {
        if event.exchange != Exchange::Oanda {
            return Vec::new();
        }
        let key = event.pair.0.clone();
        let Some(history) = self.history.get(&key) else {
            return Vec::new();
        };
        let highs = Self::highs(history);
        let lows = Self::lows(history);
        let Some((exit_low, exit_high)) =
            indicators::donchian_channel(&highs, &lows, EXIT_CHANNEL, false)
        else {
            return Vec::new();
        };

        let close = event.candle.close;
        let mut exits = Vec::new();
        for pos in positions {
            if pos.trade.strategy_name != self.name {
                continue;
            }
            if pos.trade.pair.0 != key {
                continue;
            }
            let trailing_break = match pos.trade.direction {
                Direction::Long => close < exit_low,
                Direction::Short => close > exit_high,
            };
            if trailing_break {
                exits.push(ExitSignal {
                    trade_id: pos.trade.id,
                    reason: StrategyExitReason::TrailingChannel,
                });
            }
        }
        exits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strat_with_alloc(alloc: Decimal) -> DonchianTrendFxV1 {
        DonchianTrendFxV1::new(
            "donchian_trend_fx_test".to_string(),
            vec![Pair::new("USD_JPY")],
            alloc,
        )
    }

    #[test]
    fn constructor_stores_allocation_pct() {
        let s = strat_with_alloc(dec!(0.5));
        assert_eq!(s.allocation_pct, dec!(0.5));

        let s = strat_with_alloc(dec!(0.8));
        assert_eq!(s.allocation_pct, dec!(0.8));
    }

    #[test]
    fn baseline_atr_requires_minimum_history() {
        let s = strat_with_alloc(dec!(0.5));
        let mut history: VecDeque<Candle> = VecDeque::new();
        // Add exactly ATR_BASELINE_BARS + ATR_PERIOD + 1 bars — one
        // short of the guard. Must return None.
        let total = ATR_BASELINE_BARS + ATR_PERIOD + 1;
        for i in 0..total {
            history.push_back(Candle {
                pair: Pair::new("USD_JPY"),
                exchange: Exchange::Oanda,
                timeframe: "M15".to_string(),
                open: dec!(150.0),
                high: dec!(150.05),
                low: dec!(149.95),
                close: dec!(150.00) + Decimal::from(i as i64) / dec!(100),
                volume: Some(100),
                timestamp: chrono::Utc::now(),
            });
        }
        assert!(DonchianTrendFxV1::baseline_atr(&history).is_none());
        let _ = s; // avoid unused warning
    }
}
```

- [ ] **Step 3: cargo check + clippy + test**

```bash
source ~/.cargo/env
cargo check -p auto-trader-strategy
cargo clippy -p auto-trader-strategy -- -D warnings
cargo test -p auto-trader-strategy donchian_trend_fx
```

All must pass. The 2 unit tests should run.

- [ ] **Step 4: Commit**

```bash
git add crates/strategy/src/donchian_trend_fx.rs crates/strategy/src/lib.rs
git commit -m "feat(strategy): donchian_trend_fx_v1 (ATR SL + 10-bar trailing exit)"
```

---

## Task 5: main.rs wiring — OANDA raw tick drain, FX position monitor, strategy registration

Biggest surgery of the PR. Reuses the bitflyer pattern for OANDA.

**Files:**
- Modify: `crates/app/src/main.rs`

- [ ] **Step 1: Rename the bitflyer channel variable for clarity**

Find the line (around `main.rs:60`):

```rust
    let (raw_tick_tx, mut raw_tick_rx) =
        mpsc::channel::<auto_trader_market::bitflyer::RawTick>(1024);
```

Replace with:

```rust
    let (bf_raw_tick_tx, mut bf_raw_tick_rx) =
        mpsc::channel::<auto_trader_market::RawTick>(1024);
    let (oanda_raw_tick_tx, mut oanda_raw_tick_rx) =
        mpsc::channel::<auto_trader_market::RawTick>(1024);
```

Note the type path moved from `bitflyer::RawTick` to the crate root `auto_trader_market::RawTick` (Task 1 moved it).

- [ ] **Step 2: Update every reference to `raw_tick_tx` and `raw_tick_rx` to use the `bf_` prefix**

Search the file:

```bash
grep -n "raw_tick_tx\|raw_tick_rx" crates/app/src/main.rs
```

Replace each remaining `raw_tick_tx` with `bf_raw_tick_tx` and `raw_tick_rx` with `bf_raw_tick_rx`. Concretely, two call sites inside the existing bitflyer drain task and the `.with_raw_tick_sink(raw_tick_tx.clone())` call on `BitflyerMonitor::new(...)` need to become `bf_raw_tick_tx.clone()`.

- [ ] **Step 3: Wire `.with_raw_tick_sink` into the fx_monitor builder**

Find the fx_monitor block (around `main.rs:74-87`):

```rust
    let fx_monitor: Option<MarketMonitor> = if !fx_pairs.is_empty() {
        match (std::env::var("OANDA_API_KEY"), config.oanda.as_ref()) {
            (Ok(api_key), Some(oanda_config)) if !api_key.trim().is_empty() => {
                for p in &fx_pairs {
                    expected_feeds.push(crate::price_store::FeedKey::new(
                        auto_trader_core::types::Exchange::Oanda,
                        p.clone(),
                    ));
                }
                let account_id = std::env::var("OANDA_ACCOUNT_ID")
                    .unwrap_or_else(|_| oanda_config.account_id.clone());
                let oanda = OandaClient::new(&oanda_config.api_url, &account_id, &api_key)?;
                Some(MarketMonitor::new(
                    oanda,
                    fx_pairs,
                    config.monitor.interval_secs,
                    FX_TIMEFRAME,
                    price_tx.clone(),
                )
                .with_db(pool.clone()))
            }
```

Replace the `MarketMonitor::new(...).with_db(pool.clone())` chain with:

```rust
                Some(MarketMonitor::new(
                    oanda,
                    fx_pairs,
                    config.monitor.interval_secs,
                    FX_TIMEFRAME,
                    price_tx.clone(),
                )
                .with_db(pool.clone())
                .with_raw_tick_sink(oanda_raw_tick_tx.clone()))
```

- [ ] **Step 4: Add the OANDA drain task next to the bitflyer drain task**

Find the existing bitflyer drain task (around `main.rs:518-530`, just after the `let price_store = crate::price_store::PriceStore::new(expected_feeds);` line). It looks like:

```rust
    let raw_tick_store = price_store.clone();
    let _raw_tick_drain_handle = tokio::spawn(async move {
        while let Some((pair, price, ts)) = raw_tick_rx.recv().await {
            raw_tick_store
                .update(
                    crate::price_store::FeedKey::new(
                        auto_trader_core::types::Exchange::BitflyerCfd,
                        pair,
                    ),
                    crate::price_store::LatestTick { price, ts },
                )
                .await;
        }
    });
```

Rename `raw_tick_store` → `bf_raw_tick_store`, `raw_tick_rx` → `bf_raw_tick_rx`, and `_raw_tick_drain_handle` → `_bf_raw_tick_drain_handle`, so the block becomes:

```rust
    let bf_raw_tick_store = price_store.clone();
    let _bf_raw_tick_drain_handle = tokio::spawn(async move {
        while let Some((pair, price, ts)) = bf_raw_tick_rx.recv().await {
            bf_raw_tick_store
                .update(
                    crate::price_store::FeedKey::new(
                        auto_trader_core::types::Exchange::BitflyerCfd,
                        pair,
                    ),
                    crate::price_store::LatestTick { price, ts },
                )
                .await;
        }
    });
```

Immediately after it, add the OANDA drain task:

```rust
    let oanda_raw_tick_store = price_store.clone();
    let _oanda_raw_tick_drain_handle = tokio::spawn(async move {
        while let Some((pair, price, ts)) = oanda_raw_tick_rx.recv().await {
            oanda_raw_tick_store
                .update(
                    crate::price_store::FeedKey::new(
                        auto_trader_core::types::Exchange::Oanda,
                        pair,
                    ),
                    crate::price_store::LatestTick { price, ts },
                )
                .await;
        }
    });
```

- [ ] **Step 5: Replace the drain-only FX position monitor with a real one**

Find the block (around `main.rs:582-587`):

```rust
    // FX position monitor removed: FX paper trading is currently disabled.
    // Drain the forwarded FX price channel so senders do not block.
    let mut price_monitor_rx = price_monitor_rx;
    let pos_monitor_handle = tokio::spawn(async move {
        while price_monitor_rx.recv().await.is_some() {}
    });
```

Replace the entire block with a real FX position monitor that mirrors the crypto one (lines ~593-700). The key differences:

- Filter on `Exchange::Oanda` instead of `Exchange::BitflyerCfd`
- Pass `Exchange::Oanda` to `PaperTrader::new`
- Rename internal variables to `fx_*` for readability

```rust
    // FX position monitor — single task, DB-driven.
    //
    // Mirrors the crypto position monitor below: re-read the open-
    // trade list on every price tick and close positions whose
    // SL/TP/time-limit has been hit. Filters by Exchange::Oanda so
    // crypto trades never touch this path.
    let fx_monitor_pool = pool.clone();
    let fx_monitor_trade_tx = trade_tx.clone();
    let mut price_monitor_rx = price_monitor_rx;
    let pos_monitor_handle = tokio::spawn(async move {
        while let Some(event) = price_monitor_rx.recv().await {
            let current_price = event.candle.close;
            let open_trades = match auto_trader_db::trades::list_open_with_account_name(
                &fx_monitor_pool,
            ).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("fx monitor: failed to list open trades: {e}");
                    continue;
                }
            };
            for owned in open_trades {
                let trade = owned.trade;
                if trade.exchange != Exchange::Oanda || trade.pair != event.pair {
                    continue;
                }
                let Some(account_id) = trade.paper_account_id else {
                    continue;
                };
                let now = chrono::Utc::now();
                let time_limit_hit = trade
                    .max_hold_until
                    .is_some_and(|deadline| now >= deadline);

                let mut exit_reason = match trade.direction {
                    Direction::Long => {
                        if current_price <= trade.stop_loss {
                            Some(auto_trader_core::types::ExitReason::SlHit)
                        } else if current_price >= trade.take_profit {
                            Some(auto_trader_core::types::ExitReason::TpHit)
                        } else {
                            None
                        }
                    }
                    Direction::Short => {
                        if current_price >= trade.stop_loss {
                            Some(auto_trader_core::types::ExitReason::SlHit)
                        } else if current_price <= trade.take_profit {
                            Some(auto_trader_core::types::ExitReason::TpHit)
                        } else {
                            None
                        }
                    }
                };
                if exit_reason.is_none() && time_limit_hit {
                    exit_reason =
                        Some(auto_trader_core::types::ExitReason::StrategyTimeLimit);
                }
                if let Some(reason) = exit_reason {
                    let exit_price = match reason {
                        auto_trader_core::types::ExitReason::SlHit => trade.stop_loss,
                        auto_trader_core::types::ExitReason::TpHit => trade.take_profit,
                        _ => current_price,
                    };
                    let trader = PaperTrader::new(
                        fx_monitor_pool.clone(),
                        Exchange::Oanda,
                        account_id,
                    );
                    match trader
                        .close_position(&trade.id.to_string(), reason, exit_price)
                        .await
                    {
                        Ok(closed_trade) => {
                            tracing::info!(
                                "fx position closed: {} {} {:?} at {} ({:?})",
                                closed_trade.strategy_name,
                                closed_trade.pair,
                                closed_trade.direction,
                                exit_price,
                                reason
                            );
                            let _ = fx_monitor_trade_tx
                                .send(auto_trader_core::event::TradeEvent {
                                    trade: closed_trade,
                                    action: auto_trader_core::event::TradeAction::Closed {
                                        exit_price,
                                        exit_reason: reason,
                                    },
                                })
                                .await;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "fx monitor close_position failed for trade {}: {e}",
                                trade.id
                            );
                        }
                    }
                }
            }
        }
    });
```

The variable `pos_monitor_handle` is preserved (same name as before) so any downstream join-handle logic continues to work.

- [ ] **Step 6: Register `donchian_trend_fx_*` strategies**

Find the strategy registration match (around `main.rs:156`):

```rust
        match sc.name.as_str() {
            name if name.starts_with("swing_llm") => {
                // ...
            }
            name if name.starts_with("bb_mean_revert") => {
                // ...
            }
            name if name.starts_with("donchian_trend") => {
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                engine.add_strategy(
                    Box::new(auto_trader_strategy::donchian_trend::DonchianTrendV1::new(
                        sc.name.clone(),
                        pairs,
                    )),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            name if name.starts_with("squeeze_momentum") => {
                // ...
            }
            other => {
                tracing::warn!("unknown strategy: {other}, skipping");
            }
        }
```

Insert a new `name if name.starts_with("donchian_trend_fx")` arm **BEFORE** the existing `name if name.starts_with("donchian_trend")` arm (Rust match guards match top-to-bottom, so the FX arm must come first or the generic `donchian_trend` arm will capture it):

```rust
            name if name.starts_with("donchian_trend_fx") => {
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                let allocation_pct = sc
                    .params
                    .get("allocation_pct")
                    .and_then(|v| v.as_float())
                    .and_then(|f| Decimal::try_from(f).ok())
                    .unwrap_or(dec!(0.5));
                engine.add_strategy(
                    Box::new(
                        auto_trader_strategy::donchian_trend_fx::DonchianTrendFxV1::new(
                            sc.name.clone(),
                            pairs,
                            allocation_pct,
                        ),
                    ),
                    sc.mode.clone(),
                );
                tracing::info!(
                    "strategy registered: {} (mode={}, allocation_pct={})",
                    sc.name,
                    sc.mode,
                    allocation_pct
                );
            }
            name if name.starts_with("donchian_trend") => {
                // ... existing crypto arm unchanged ...
            }
```

If `dec!` or `Decimal` are not already in scope at this location, they are already imported at the top of main.rs — verify with:

```bash
grep -n "use rust_decimal\|dec!" crates/app/src/main.rs | head -5
```

If they are not imported, add at the top:

```rust
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
```

- [ ] **Step 7: Full workspace verification**

```bash
source ~/.cargo/env
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

All must pass. Common pitfalls:

- "cannot find value `raw_tick_tx`" — Step 2 missed a reference
- "expected `mpsc::Sender<RawTick>`, found `mpsc::Sender<bitflyer::RawTick>`" — the `with_raw_tick_sink` signature on `BitflyerMonitor` still uses `bitflyer::RawTick`. Update its type to `crate::RawTick`
- "use of moved value: `oanda_raw_tick_tx`" — the builder chain moved it; make sure you wrote `.clone()`

- [ ] **Step 8: Commit**

```bash
git add crates/app/src/main.rs
git commit -m "feat(app): wire OANDA raw tick drain, FX position monitor, donchian_trend_fx registration"
```

---

## Task 6: Config + migration seed

**Files:**
- Modify: `config/default.toml`
- Create: `migrations/20260409000001_fx_paper_accounts_seed.sql`

- [ ] **Step 1: Scope `pairs.fx` to USD/JPY only**

Find the `[pairs]` section (around line 16):

```toml
[pairs]
fx = ["USD_JPY", "EUR_USD"]
crypto = ["FX_BTC_JPY"]
```

Replace with:

```toml
[pairs]
# Scoped to USD_JPY for the initial FX enablement PR.
# EUR_JPY and GBP_JPY are planned as the immediate follow-up PR
# (see docs/superpowers/specs/2026-04-09-fx-paper-trading-design.md).
fx = ["USD_JPY"]
crypto = ["FX_BTC_JPY"]
```

- [ ] **Step 2: Add the two `[[strategies]]` entries**

Find the existing `[[strategies]]` block for `squeeze_momentum_v1` (around lines 88-94):

```toml
[[strategies]]
name = "squeeze_momentum_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY"]
# 攻め (high risk).
params = { bb_period = 20, kc_period = 20, atr_period = 14, ema_trail_period = 21, squeeze_bars = 6 }
```

Immediately after that block, add:

```toml
[[strategies]]
name = "donchian_trend_fx_normal"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]
# FX 通常: allocation 0.50 (証拠金維持率 SL 後 200%、保守)
params = { allocation_pct = 0.50 }

[[strategies]]
name = "donchian_trend_fx_aggressive"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]
# FX 攻め: allocation 0.80 (証拠金維持率 SL 後 120%、フル寄り)
params = { allocation_pct = 0.80 }
```

- [ ] **Step 3: Create the migration file**

Write `migrations/20260409000001_fx_paper_accounts_seed.sql`:

```sql
-- FX paper accounts: 2 balances × 2 allocations = 4 accounts.
-- All run donchian_trend_fx_* on USD/JPY with leverage 25x.
-- UUID prefix b0000000-... to distinguish from crypto (a0000000-...).
--
-- ON CONFLICT DO NOTHING so this is safe to re-apply against an
-- environment where the rows were inserted via REST API instead.

-- Step 1: register the two new FX strategies in the strategies
-- catalog so paper_accounts can reference them. The strategies
-- catalog has an FK check enforced by the paper_accounts insert
-- below, so the catalog rows must exist first.
INSERT INTO strategies (name, display_name, category, risk_level, description, algorithm, default_params)
VALUES
    (
        'donchian_trend_fx_normal',
        'FX 標準ブレイクアウト 通常 (Donchian FX)',
        'fx',
        'medium',
        '20 本 Donchian ブレイク + ATR フィルタ。USD/JPY M15、allocation 50% で保守運用。',
        $md$
## 想定相場
中規模〜大規模トレンド (USD/JPY M15)

## エントリー
- **Long**: 終値が直近 20 本高値を上抜け + ATR(14) > 直近 50 本平均 ATR
- **Short**: ミラー (20 本安値下抜け + 同条件)

## 損切
- **2 × ATR(14)** 距離 (Turtle "N" stop)

## 利確 (動的)
- **Long**: 終値が直近 10 本安値を下抜けたら決済
- **Short**: 終値が直近 10 本高値を上抜けたら決済

## Allocation
- **0.50** (通常)。証拠金維持率 SL 後 200%、余裕を持って保守運用。

## Max hold
- **72 時間** (FX トレンドは crypto より長期)

## 想定スペック
- 想定 R:R: 1:2 以上 (トレイリング次第)
- 想定勝率: 35-45%
$md$,
        '{"entry_channel": 20, "exit_channel": 10, "atr_period": 14, "atr_sl_mult": 2.0, "allocation_pct": 0.50}'::jsonb
    ),
    (
        'donchian_trend_fx_aggressive',
        'FX 標準ブレイクアウト 攻め (Donchian FX)',
        'fx',
        'high',
        '20 本 Donchian ブレイク + ATR フィルタ。USD/JPY M15、allocation 80% で攻撃運用。',
        $md$
## 想定相場
中規模〜大規模トレンド (USD/JPY M15)

## エントリー・損切・利確・Max hold
`donchian_trend_fx_normal` と同じ。

## Allocation
- **0.80** (攻め)。証拠金維持率 SL 後 120%、フル寄り。ATR 拡大時には維持率低下リスクあり。

## 想定スペック
- 想定 R:R: 1:2 以上 (トレイリング次第)
- 想定勝率: 35-45%
- リスク: 通常版の 1.6 倍 (allocation 比)
$md$,
        '{"entry_channel": 20, "exit_channel": 10, "atr_period": 14, "atr_sl_mult": 2.0, "allocation_pct": 0.80}'::jsonb
    )
ON CONFLICT (name) DO NOTHING;

-- Step 2: seed the 4 paper accounts.
INSERT INTO paper_accounts (
    id, name, exchange, initial_balance, current_balance,
    currency, leverage, strategy, account_type, created_at, updated_at
) VALUES
    ('b0000000-0000-0000-0000-000000000010', 'fx_small_normal_v1',
     'oanda', 30000, 30000, 'JPY', 25,
     'donchian_trend_fx_normal', 'paper', NOW(), NOW()),
    ('b0000000-0000-0000-0000-000000000011', 'fx_small_aggressive_v1',
     'oanda', 30000, 30000, 'JPY', 25,
     'donchian_trend_fx_aggressive', 'paper', NOW(), NOW()),
    ('b0000000-0000-0000-0000-000000000012', 'fx_standard_normal_v1',
     'oanda', 100000, 100000, 'JPY', 25,
     'donchian_trend_fx_normal', 'paper', NOW(), NOW()),
    ('b0000000-0000-0000-0000-000000000013', 'fx_standard_aggressive_v1',
     'oanda', 100000, 100000, 'JPY', 25,
     'donchian_trend_fx_aggressive', 'paper', NOW(), NOW())
ON CONFLICT (id) DO NOTHING;
```

- [ ] **Step 4: Full workspace verification**

```bash
source ~/.cargo/env
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

All must pass.

- [ ] **Step 5: Commit**

```bash
git add config/default.toml migrations/20260409000001_fx_paper_accounts_seed.sql
git commit -m "feat(config): add USD_JPY donchian_trend_fx strategies + 4 paper accounts"
```

---

## Task 7: Integration verification + manual smoke test

**Files:** None (verification only)

- [ ] **Step 1: Rebuild docker image**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
docker compose build auto-trader
```

Expected: build succeeds.

- [ ] **Step 2: Verify `.env` has `OANDA_API_KEY` + `OANDA_ACCOUNT_ID`**

Ask the user to confirm they populated `.env` before restarting. Do NOT proceed without confirmation — a missing key silently disables the FX monitor and the smoke test below will pass "for the wrong reason" (no feed = no update = missing, but ALSO no Oanda entry in health because expected_feeds stays empty).

- [ ] **Step 3: Restart container**

```bash
docker compose up -d auto-trader
sleep 10
docker logs auto-trader-auto-trader-1 --tail 40
```

Expected log signals:

- `OANDA ... connected` or at least no `OANDA not configured` warning
- `strategy registered: donchian_trend_fx_normal (mode=paper, allocation_pct=0.5)`
- `strategy registered: donchian_trend_fx_aggressive (mode=paper, allocation_pct=0.8)`
- `API server listening on 0.0.0.0:3001`
- `bitflyer websocket connected`

- [ ] **Step 4: Smoke test the API endpoints**

```bash
curl -s http://localhost:3001/api/health/market-feed | python3 -m json.tool
```

Expected: output includes an entry like `{"exchange":"oanda","pair":"USD_JPY","status":"healthy","last_tick_age_secs": <small number>}`. If `status: "missing"`, wait 20 seconds and retry — the first tick may take a few seconds after process start.

```bash
curl -s http://localhost:3001/api/market/prices | python3 -m json.tool
```

Expected: includes an entry for `oanda / USD_JPY` with a realistic USD/JPY mid price.

```bash
curl -s http://localhost:3001/api/paper-accounts | python3 -m json.tool | grep -E 'name|exchange'
```

Expected: the 4 new FX accounts (`fx_small_normal_v1`, `fx_small_aggressive_v1`, `fx_standard_normal_v1`, `fx_standard_aggressive_v1`) appear in the list with `exchange: "oanda"`.

- [ ] **Step 5: DB sanity check**

```bash
docker exec auto-trader-db-1 psql -U auto-trader -d auto_trader -c "SELECT name, exchange, initial_balance, leverage, strategy FROM paper_accounts WHERE exchange='oanda' ORDER BY name;"
```

Expected: 4 rows with the correct `strategy` column mapping (2 with `donchian_trend_fx_normal`, 2 with `donchian_trend_fx_aggressive`).

```bash
docker exec auto-trader-db-1 psql -U auto-trader -d auto_trader -c "SELECT name, category, risk_level FROM strategies WHERE name LIKE 'donchian_trend_fx%';"
```

Expected: 2 rows (normal + aggressive), `category = 'fx'`.

- [ ] **Step 6: Visual dashboard smoke test**

Open the dashboard in a browser. Walk through:

- [ ] Navigation order unchanged (概要 / ポジション / トレード / ...)
- [ ] Market feed health banner is NOT showing (banner green = healthy or hidden)
- [ ] Positions tab empty at first (no open FX positions yet), accounts tab shows the 4 new FX accounts
- [ ] No console / network errors in browser dev tools

- [ ] **Step 7: Wait for first signal (optional, user-discretion)**

`donchian_trend_fx_*` requires `ENTRY_CHANNEL + ATR_BASELINE_BARS + ATR_PERIOD + 1 = 85` M15 bars of history = ~21 hours of live data before the first entry can fire. (Warmup from the candles REST endpoint handles most of this on startup, so the first signal is likely within a few hours of startup rather than 21 hours.)

Check after some hours:

```bash
docker exec auto-trader-db-1 psql -U auto-trader -d auto_trader -c "SELECT strategy_name, COUNT(*) FROM trades WHERE exchange='oanda' GROUP BY strategy_name;"
```

Expected (after enough time + trending market): non-zero counts. If still zero after a full day of trending market, re-examine the breakout filter — it may be too strict for USD/JPY M15.

- [ ] **Step 8: Report findings back to the user**

Summarize smoke test results. If anything failed, do NOT commit/push — fix first.

---

## Post-Implementation

1. Run the `code-review` skill flow: local codex review loop, address findings, push, create PR with `gh pr create`, request Copilot review via Web UI (API path is unreliable — remind the user to click through GitHub UI if needed), address Copilot findings in 1 round.
2. Do NOT merge. The user merges.
3. Immediate follow-up: the `project_fx_pair_expansion` memory flags that EUR_JPY + GBP_JPY should land in a second PR right after this one. Do not bundle them.
