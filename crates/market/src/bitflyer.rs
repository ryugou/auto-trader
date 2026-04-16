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
        let mut builders: HashMap<String, CandleBuilder> = HashMap::new();
        for pair in &self.pairs {
            builders.insert(
                pair.0.clone(),
                CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, self.timeframe.clone()),
            );
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
                &mut closes_map,
                &mut highs_map,
                &mut lows_map,
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

#[allow(clippy::too_many_arguments)]
async fn connect_and_stream(
    ws_url: &str,
    pairs: &[Pair],
    pool: Option<&PgPool>,
    tick_tx: &mpsc::Sender<(FeedKey, LatestTick)>,
    price_tx: &mpsc::Sender<PriceEvent>,
    builders: &mut HashMap<String, CandleBuilder>,
    closes_map: &mut HashMap<String, Vec<Decimal>>,
    highs_map: &mut HashMap<String, Vec<Decimal>>,
    lows_map: &mut HashMap<String, Vec<Decimal>>,
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

        // on_tick returns completed candle when period boundary is crossed
        let from_tick = builder.on_tick(price, size, ts, best_bid, best_ask);
        let from_complete = builder.try_complete(ts, best_bid, best_ask);
        let completed = from_tick.or(from_complete);

        if let Some(candle) = completed {
            if let Some(pool) = pool
                && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &candle).await
            {
                tracing::warn!("failed to save crypto candle: {e}");
            }

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

            let event = PriceEvent {
                pair: candle.pair.clone(),
                exchange: Exchange::BitflyerCfd,
                timestamp: candle.timestamp,
                candle,
                indicators: indicator_map,
            };

            if price_tx.send(event).await.is_err() {
                tracing::info!("price channel closed, stopping bitflyer monitor");
                return Ok(());
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
