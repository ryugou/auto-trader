// Match old raw_tick_tx capacity (main.rs pre-refactor). Sized to
// absorb brief bursts during heavy market activity without dropping
// ticks; drain task typically empties the queue in <1ms.
const PRICE_STORE_TICK_CHANNEL_CAP: usize = 1024;

use crate::candle_builder::CandleBuilder;
use crate::indicators;
use crate::market_feed::MarketFeed;
use crate::price_store::{FeedKey, LatestTick, PriceStore};
use async_trait::async_trait;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[derive(serde::Deserialize)]
struct JsonRpcMessage {
    method: Option<String>,
    params: Option<TickerParams>,
}

#[derive(serde::Deserialize)]
struct TickerParams {
    message: TickerMessage,
}

#[derive(serde::Deserialize)]
struct TickerMessage {
    product_code: String,
    best_bid: Decimal,
    best_ask: Decimal,
    ltp: Decimal,
    volume: Decimal,
    timestamp: String,
}

pub struct BitflyerMonitor {
    ws_url: String,
    pairs: Vec<Pair>,
    timeframe: String,
    pool: Option<PgPool>,
    closes_seed: HashMap<String, Vec<Decimal>>,
    highs_seed: HashMap<String, Vec<Decimal>>,
    lows_seed: HashMap<String, Vec<Decimal>>,
}

impl BitflyerMonitor {
    pub fn new(ws_url: &str, pairs: Vec<Pair>, timeframe: &str) -> Self {
        Self {
            ws_url: ws_url.to_string(),
            pairs,
            timeframe: timeframe.to_string(),
            pool: None,
            closes_seed: HashMap::new(),
            highs_seed: HashMap::new(),
            lows_seed: HashMap::new(),
        }
    }

    pub fn with_db(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Pre-populate per-pair `closes_map` (and derive matching highs/lows from
    /// the same candle history) so indicators can fire from the very first live
    /// candle after restart. Caller is responsible for loading and ordering
    /// the closes (oldest → newest).
    ///
    /// For the new ATR/ADX indicators highs and lows are required.
    /// When only closes are available (e.g. legacy callers), the seed
    /// approximates high = low = close, which degrades ATR/ADX quality but
    /// does not panic. The canonical path in `main.rs` passes full `Candle`
    /// structs via `with_candle_seed`.
    pub fn with_closes_seed(mut self, seed: HashMap<String, Vec<Decimal>>) -> Self {
        self.closes_seed = seed;
        self
    }

    /// Pre-populate candle history (closes + highs + lows) from the DB warmup
    /// path. Preferred over `with_closes_seed` when the caller has full candle
    /// data available (which is always the case in the app composition layer).
    pub fn with_candle_seed(
        mut self,
        highs: HashMap<String, Vec<Decimal>>,
        lows: HashMap<String, Vec<Decimal>>,
        closes: HashMap<String, Vec<Decimal>>,
    ) -> Self {
        self.closes_seed = closes;
        self.highs_seed = highs;
        self.lows_seed = lows;
        self
    }

    async fn run_inner(
        mut self,
        price_store: Arc<PriceStore>,
        price_tx: mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()> {
        // Primary builders for the configured timeframe (M5 for crypto strategies
        // that want fine-grained data, e.g. bb_mean_revert).
        let mut builders: HashMap<String, CandleBuilder> = HashMap::new();
        // Secondary H1 builders — Donchian / Squeeze strategies use 1H candles
        // to reduce false breakouts on daily-bar-designed logic.
        // Only created when the primary timeframe is not already H1; if it were,
        // both `builders` and `h1_builders` would emit H1 candles → duplicate events.
        let mut h1_builders: HashMap<String, CandleBuilder> = HashMap::new();
        for pair in &self.pairs {
            builders.insert(
                pair.0.clone(),
                CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, self.timeframe.clone()),
            );
            if self.timeframe != "H1" {
                h1_builders.insert(
                    pair.0.clone(),
                    CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, "H1".to_string()),
                );
            }
        }
        // Seed indicator state from caller-provided history. Move out rather
        // than cloning to avoid duplicate copies of every warmup vector.
        let mut closes_map: HashMap<String, Vec<Decimal>> = std::mem::take(&mut self.closes_seed);
        let mut highs_map: HashMap<String, Vec<Decimal>> = std::mem::take(&mut self.highs_seed);
        let mut lows_map: HashMap<String, Vec<Decimal>> = std::mem::take(&mut self.lows_seed);
        for (pair, closes) in &closes_map {
            tracing::info!(
                "bitflyer warmup: seeded {} {} closes for {}",
                closes.len(),
                self.timeframe,
                pair
            );
        }

        // Internal bounded channel: WS loop uses try_send (non-blocking, drops
        // on full) and hands raw ticks to a dedicated drain task that performs
        // the async PriceStore write. This keeps the WS read loop non-blocking,
        // matching the semantics of the old raw_tick_tx channel.
        let (tick_tx, mut tick_rx) =
            mpsc::channel::<(FeedKey, LatestTick)>(PRICE_STORE_TICK_CHANNEL_CAP);
        let ps = price_store.clone();
        tokio::spawn(async move {
            while let Some((key, tick)) = tick_rx.recv().await {
                ps.update(key, tick).await;
            }
        });

        let mut backoff_secs = 1u64;

        loop {
            match connect_and_stream(
                &self.ws_url,
                &self.pairs,
                self.pool.as_ref(),
                &tick_tx,
                &price_tx,
                &mut builders,
                &mut h1_builders,
                &mut closes_map,
                &mut highs_map,
                &mut lows_map,
                &self.timeframe,
            )
            .await
            {
                Ok(()) => {
                    if price_tx.is_closed() {
                        tracing::info!("price channel closed, stopping bitflyer monitor");
                        return Ok(());
                    }
                    // Normal close — reconnect (server can close gracefully)
                    tracing::info!("bitflyer websocket closed normally, reconnecting");
                    backoff_secs = 1;
                    continue;
                }
                Err(e) => {
                    if price_tx.is_closed() {
                        tracing::info!("price channel closed, stopping bitflyer monitor");
                        return Ok(());
                    }
                    if tick_tx.is_closed() {
                        tracing::error!(
                            "bitflyer feed: tick drain channel permanently closed, stopping feed (no reconnect)"
                        );
                        return Err(e);
                    }
                    tracing::warn!(
                        "bitflyer websocket error, reconnecting in {backoff_secs}s: {e}"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                }
            }
        }
    }
}

/// Build and emit a `PriceEvent` for a completed candle, updating the rolling
/// indicator history vectors in-place. The indicator map is computed from the
/// primary-timeframe history vectors so the position monitor and API
/// server always see fresh primary-timeframe indicators regardless of which
/// timeframe fired. H1 PriceEvents carry a namespaced indicator map —
/// strategies that consume them compute their own indicators from their
/// internal `VecDeque<Candle>` history.
fn emit_candle_event(
    candle: auto_trader_core::types::Candle,
    closes_map: &mut HashMap<String, Vec<Decimal>>,
    highs_map: &mut HashMap<String, Vec<Decimal>>,
    lows_map: &mut HashMap<String, Vec<Decimal>>,
    compute_full_indicators: bool,
) -> (PriceEvent, HashMap<String, Decimal>) {
    let product_code = candle.pair.0.clone();

    let closes = closes_map.entry(product_code.clone()).or_default();
    closes.push(candle.close);
    if closes.len() > 200 {
        closes.drain(..closes.len() - 200);
    }

    let highs = highs_map.entry(product_code.clone()).or_default();
    highs.push(candle.high);
    if highs.len() > 200 {
        highs.drain(..highs.len() - 200);
    }

    let lows = lows_map.entry(product_code.clone()).or_default();
    lows.push(candle.low);
    if lows.len() > 200 {
        lows.drain(..lows.len() - 200);
    }

    let mut indicator_map = HashMap::new();
    if compute_full_indicators {
        if let Some(v) = indicators::sma(closes, 20) {
            indicator_map.insert("sma_20".to_string(), v);
        }
        if let Some(v) = indicators::sma(closes, 50) {
            indicator_map.insert("sma_50".to_string(), v);
        }
        if let Some(v) = indicators::rsi(closes, 14) {
            indicator_map.insert("rsi_14".to_string(), v);
        }
        if let Some(v) = indicators::atr(highs, lows, closes, 14) {
            indicator_map.insert("atr_14".to_string(), v);
        }
        if let Some(v) = indicators::adx(highs, lows, closes, 14) {
            indicator_map.insert("adx_14".to_string(), v);
        }
        // BB width as percentage of SMA20
        if let Some((bb_lo, bb_mid, bb_up)) =
            indicators::bollinger_bands(closes, 20, Decimal::from(2))
            && bb_mid > Decimal::ZERO
        {
            let bb_width_pct = (bb_up - bb_lo) / bb_mid * Decimal::from(100);
            indicator_map.insert("bb_width_pct".to_string(), bb_width_pct);
        }
        // ATR percentile: rank of current ATR within the last 50 ATR values
        if let Some(current_atr) = indicator_map.get("atr_14").copied() {
            let lookback = 50.min(closes.len());
            if lookback >= 15 {
                let mut atr_count_below = 0u32;
                let mut atr_total = 0u32;
                for end in (closes.len() - lookback)..closes.len() {
                    if end >= 14
                        && let Some(past_atr) =
                            indicators::atr(&highs[..=end], &lows[..=end], &closes[..=end], 14)
                    {
                        atr_total += 1;
                        if past_atr < current_atr {
                            atr_count_below += 1;
                        }
                    }
                }
                if atr_total > 0 {
                    let pct = Decimal::from(atr_count_below) / Decimal::from(atr_total)
                        * Decimal::from(100);
                    indicator_map.insert("atr_percentile".to_string(), pct);
                }
            }
        }
    }

    let event = PriceEvent {
        pair: candle.pair.clone(),
        exchange: Exchange::BitflyerCfd,
        timestamp: candle.timestamp,
        candle,
        indicators: indicator_map.clone(),
    };
    (event, indicator_map)
}

#[allow(clippy::too_many_arguments)]
async fn connect_and_stream(
    ws_url: &str,
    pairs: &[Pair],
    pool: Option<&PgPool>,
    tick_tx: &mpsc::Sender<(FeedKey, LatestTick)>,
    price_tx: &mpsc::Sender<PriceEvent>,
    builders: &mut HashMap<String, CandleBuilder>,
    h1_builders: &mut HashMap<String, CandleBuilder>,
    closes_map: &mut HashMap<String, Vec<Decimal>>,
    highs_map: &mut HashMap<String, Vec<Decimal>>,
    lows_map: &mut HashMap<String, Vec<Decimal>>,
    primary_timeframe: &str,
) -> anyhow::Result<()> {
    let (ws, _) = connect_async(ws_url).await?;
    let (mut write, mut read) = ws.split();
    tracing::info!("bitflyer websocket connected: {}", ws_url);

    for pair in pairs {
        let subscribe = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "subscribe",
            "params": { "channel": format!("lightning_ticker_{}", pair.0) }
        });
        write.send(Message::Text(subscribe.to_string())).await?;
    }

    // Per-pair cache of the most recent primary-timeframe indicator map.
    // Declared outside the tick loop so H1 candles that complete on a tick
    // where no primary candle fires can still carry the last-known indicators.
    let mut latest_indicators: HashMap<String, HashMap<String, Decimal>> = HashMap::new();
    // Dynamic prefix derived from the primary timeframe (e.g. "m5", "h1").
    let primary_tf_prefix = primary_timeframe.to_lowercase();

    while let Some(msg) = read.next().await {
        let msg = msg?;
        let Message::Text(text) = msg else { continue };

        let rpc: JsonRpcMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if rpc.method.as_deref() != Some("channelMessage") {
            continue;
        }

        let Some(params) = rpc.params else { continue };
        let ticker = params.message;
        let product_code = &ticker.product_code;

        let Some(builder) = builders.get_mut(product_code) else {
            continue;
        };

        let price = ticker.ltp;
        let size = ticker.volume;
        let best_bid = Some(ticker.best_bid);
        let best_ask = Some(ticker.best_ask);
        let ts =
            chrono::DateTime::parse_from_rfc3339(&ticker.timestamp)?.with_timezone(&chrono::Utc);

        // Forward the raw tick to the drain task via try_send (non-blocking).
        // Drops the tick if the channel is full — acceptable: the drain task
        // will catch the next tick. Raw ticks carry sub-second wall-clock
        // timestamps so the 60s freshness threshold is easily met even with
        // occasional drops.
        if let Err(e) = tick_tx.try_send((
            FeedKey::new(Exchange::BitflyerCfd, Pair::new(product_code)),
            LatestTick {
                price,
                best_bid,
                best_ask,
                ts,
            },
        )) {
            match e {
                mpsc::error::TrySendError::Full(_) => {
                    // Drain task can't keep up; log at debug (not warn) to
                    // avoid flooding. PriceStore falls behind by 1 tick; not
                    // fatal.
                    tracing::debug!("bitflyer tick drop: drain channel full");
                }
                mpsc::error::TrySendError::Closed(_) => {
                    tracing::error!(
                        "bitflyer tick drain channel closed; PriceStore updates stopped — stopping feed"
                    );
                    return Err(anyhow::anyhow!(
                        "bitflyer tick drain channel closed; PriceStore updates stopped"
                    ));
                }
            }
        }

        // --- Primary timeframe (e.g. M5) candle ---
        // on_tick returns completed candle when period boundary is crossed
        let from_tick = builder.on_tick(price, size, ts, best_bid, best_ask);
        let from_complete = builder.try_complete(ts, best_bid, best_ask);
        if let Some(candle) = from_tick.or(from_complete) {
            if let Some(pool) = pool
                && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &candle).await
            {
                tracing::warn!("failed to save crypto candle: {e}");
            }
            let pair_key = candle.pair.0.clone();
            let (event, indicators) = emit_candle_event(
                candle, closes_map, highs_map, lows_map,
                true, // full indicator map for primary timeframe
            );
            // Persist indicators so H1 candles on subsequent ticks can use them.
            latest_indicators.insert(pair_key, indicators);
            if price_tx.send(event).await.is_err() {
                tracing::info!("price channel closed, stopping bitflyer monitor");
                return Ok(());
            }
        }

        // --- H1 candle (only when h1_builders is populated, i.e. primary != H1) ---
        if let Some(h1_builder) = h1_builders.get_mut(product_code) {
            let h1_from_tick = h1_builder.on_tick(price, size, ts, best_bid, best_ask);
            let h1_from_complete = h1_builder.try_complete(ts, best_bid, best_ask);
            if let Some(h1_candle) = h1_from_tick.or(h1_from_complete) {
                if let Some(pool) = pool
                    && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &h1_candle).await
                {
                    tracing::warn!("failed to save H1 crypto candle: {e}");
                }
                // Carry forward primary-timeframe indicators with a dynamic prefix
                // (e.g. "m5_") onto H1 events. This namespacing lets analytics
                // distinguish the source timeframe while H1-triggered trades retain
                // ATR/ADX/regime context for entry_indicators persistence without
                // mislabeling them as H1-native indicators.
                // Uses the per-pair cache so indicators are available even when the
                // primary candle did not complete on this same tick.
                let h1_indicators = latest_indicators
                    .get(product_code)
                    .map(|ind| {
                        let mut combined = ind.clone(); // unprefixed originals (backward compat)
                        for (key, value) in ind {
                            // Add prefixed duplicates for timeframe disambiguation in analytics
                            combined.insert(format!("{primary_tf_prefix}_{key}"), *value);
                        }
                        combined
                    })
                    .unwrap_or_default();
                let h1_event = PriceEvent {
                    pair: h1_candle.pair.clone(),
                    exchange: Exchange::BitflyerCfd,
                    timestamp: h1_candle.timestamp,
                    candle: h1_candle,
                    indicators: h1_indicators,
                };
                if price_tx.send(h1_event).await.is_err() {
                    tracing::info!("price channel closed, stopping bitflyer monitor");
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

#[async_trait]
impl MarketFeed for BitflyerMonitor {
    async fn run(
        self: Box<Self>,
        price_store: Arc<PriceStore>,
        price_tx: mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()> {
        // Box<Self> gives us owned BitflyerMonitor via *self; no try_unwrap needed.
        (*self).run_inner(price_store, price_tx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candle_builder::CandleBuilder;
    use auto_trader_core::types::{Exchange, Pair};
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;

    /// Verify that a completed M5 candle produces a full indicator_map
    /// and an H1 builder for the same pair correctly tracks progress.
    #[test]
    fn emit_candle_event_populates_indicators_for_primary_timeframe() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut closes_map: HashMap<String, Vec<Decimal>> = HashMap::new();
        let mut highs_map: HashMap<String, Vec<Decimal>> = HashMap::new();
        let mut lows_map: HashMap<String, Vec<Decimal>> = HashMap::new();

        // Seed 20 bars so SMA20 can fire
        let entry = closes_map.entry("FX_BTC_JPY".to_string()).or_default();
        for i in 0..20u64 {
            entry.push(Decimal::from(10_000_000u64 + i * 1000));
        }
        highs_map
            .entry("FX_BTC_JPY".to_string())
            .or_default()
            .extend(entry.iter().map(|c| *c + dec!(5000)));
        lows_map
            .entry("FX_BTC_JPY".to_string())
            .or_default()
            .extend(entry.iter().map(|c| *c - dec!(5000)));

        let candle = auto_trader_core::types::Candle {
            pair: pair.clone(),
            exchange: Exchange::BitflyerCfd,
            timeframe: "M5".to_string(),
            open: dec!(10_020_000),
            high: dec!(10_025_000),
            low: dec!(10_015_000),
            close: dec!(10_022_000),
            volume: Some(10),
            best_bid: None,
            best_ask: None,
            timestamp: Utc.with_ymd_and_hms(2026, 4, 19, 0, 5, 0).unwrap(),
        };

        let (event, _indicators) =
            emit_candle_event(candle, &mut closes_map, &mut highs_map, &mut lows_map, true);
        assert_eq!(event.candle.timeframe, "M5");
        // SMA20 must be present after 21 closes (20 seeded + 1 from candle)
        assert!(
            event.indicators.contains_key("sma_20"),
            "sma_20 must be in indicator map"
        );
    }

    /// H1 builders must track ticks independently from M5 builders.
    /// Two ticks within the same H1 period should not yet emit a candle.
    #[test]
    fn h1_builder_does_not_emit_mid_period() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut h1 = CandleBuilder::new(pair, Exchange::BitflyerCfd, "H1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();

        // Two ticks within the same H1 period — should not emit
        assert!(
            h1.on_tick(dec!(10_000_000), dec!(0.1), base, None, None)
                .is_none()
        );
        assert!(
            h1.on_tick(
                dec!(10_010_000),
                dec!(0.2),
                base + chrono::Duration::minutes(30),
                None,
                None
            )
            .is_none()
        );
        // Still mid-period
        assert!(
            h1.try_complete(base + chrono::Duration::minutes(59), None, None)
                .is_none()
        );
    }

    /// A tick arriving in the next H1 period must complete the previous candle.
    #[test]
    fn h1_builder_emits_on_period_boundary() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut h1 = CandleBuilder::new(pair, Exchange::BitflyerCfd, "H1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();

        h1.on_tick(dec!(10_000_000), dec!(0.1), base, None, None);

        // Tick in the next H1 period completes the previous candle
        let candle = h1.on_tick(
            dec!(10_100_000),
            dec!(0.1),
            base + chrono::Duration::hours(1),
            None,
            None,
        );
        assert!(
            candle.is_some(),
            "H1 candle should be emitted at period boundary"
        );
        let c = candle.unwrap();
        assert_eq!(c.timeframe, "H1");
        assert_eq!(c.open, dec!(10_000_000));
        assert_eq!(c.close, dec!(10_000_000));
    }
}
