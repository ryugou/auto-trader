const PRICE_STORE_TICK_CHANNEL_CAP: usize = 1024;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::candle_builder::CandleBuilder;
use crate::indicators;
use crate::market_feed::MarketFeed;
use crate::price_store::{FeedKey, LatestTick, PriceStore};

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

const SPOT_ONLY_PAIRS: &[&str] = &["BTC_JPY"];

fn is_spot_only(pair: &Pair) -> bool {
    SPOT_ONLY_PAIRS.contains(&pair.0.as_str())
}

impl BitflyerMonitor {
    pub fn new(ws_url: &str, pairs: Vec<Pair>, timeframe: &str) -> Self {
        let mut pairs = pairs;
        let has_fx_btc = pairs.iter().any(|p| p.0 == "FX_BTC_JPY");
        let has_spot_btc = pairs.iter().any(|p| p.0 == "BTC_JPY");
        if has_fx_btc && !has_spot_btc {
            pairs.push(Pair::new("BTC_JPY"));
        }
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

    pub fn with_closes_seed(mut self, seed: HashMap<String, Vec<Decimal>>) -> Self {
        self.closes_seed = seed;
        self
    }

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
        let mut h1_builders: HashMap<String, CandleBuilder> = HashMap::new();
        for pair in &self.pairs {
            if is_spot_only(pair) {
                continue;
            }
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
        let mut closes_map: HashMap<String, Vec<Decimal>> = std::mem::take(&mut self.closes_seed);
        let mut highs_map: HashMap<String, Vec<Decimal>> = std::mem::take(&mut self.highs_seed);
        let mut lows_map: HashMap<String, Vec<Decimal>> = std::mem::take(&mut self.lows_seed);
        let mut closes_map_h1: HashMap<String, Vec<Decimal>> = HashMap::new();
        let mut highs_map_h1: HashMap<String, Vec<Decimal>> = HashMap::new();
        let mut lows_map_h1: HashMap<String, Vec<Decimal>> = HashMap::new();
        for (pair, closes) in &closes_map {
            tracing::info!(
                pair = %pair,
                timeframe = %self.timeframe,
                count = closes.len(),
                "seeded warmup closes"
            );
        }

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
                &mut closes_map_h1,
                &mut highs_map_h1,
                &mut lows_map_h1,
                &self.timeframe,
            )
            .await
            {
                Ok(()) => {
                    if price_tx.is_closed() {
                        tracing::info!("price channel closed, stopping bitflyer monitor");
                        return Ok(());
                    }
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
                            "failed to forward ticks: drain channel closed, stopping feed without reconnect"
                        );
                        return Err(e);
                    }
                    tracing::warn!(
                        backoff_secs,
                        error = %e,
                        "failed to stream websocket messages, reconnecting"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                }
            }
        }
    }
}

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

    let indicator_map = if compute_full_indicators {
        indicators::indicator_map(closes, highs, lows)
    } else {
        HashMap::new()
    };

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
    closes_map_h1: &mut HashMap<String, Vec<Decimal>>,
    highs_map_h1: &mut HashMap<String, Vec<Decimal>>,
    lows_map_h1: &mut HashMap<String, Vec<Decimal>>,
    primary_timeframe: &str,
) -> anyhow::Result<()> {
    let (ws, _) = connect_async(ws_url).await?;
    let (mut write, mut read) = ws.split();
    tracing::info!(ws_url = %ws_url, "bitflyer websocket connected");

    for pair in pairs {
        let subscribe = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "subscribe",
            "params": { "channel": format!("lightning_ticker_{}", pair.0) }
        });
        write.send(Message::Text(subscribe.to_string())).await?;
    }

    let mut latest_indicators: HashMap<String, HashMap<String, Decimal>> = HashMap::new();
    let primary_tf_prefix = primary_timeframe.to_lowercase();

    const HEARTBEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

    loop {
        let msg = match tokio::time::timeout(HEARTBEAT_TIMEOUT, read.next()).await {
            Ok(Some(msg)) => msg?,
            Ok(None) => {
                return Ok(());
            }
            Err(_) => {
                anyhow::bail!(
                    "failed to receive websocket message: no message for {}s, treating connection as stale",
                    HEARTBEAT_TIMEOUT.as_secs()
                );
            }
        };
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

        let price = ticker.ltp;
        let size = ticker.volume;
        let best_bid = Some(ticker.best_bid);
        let best_ask = Some(ticker.best_ask);
        let ts =
            chrono::DateTime::parse_from_rfc3339(&ticker.timestamp)?.with_timezone(&chrono::Utc);

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
                    tracing::debug!("failed to send tick: drain channel full");
                }
                mpsc::error::TrySendError::Closed(_) => {
                    tracing::error!("failed to send tick: drain channel closed, stopping feed");
                    return Err(anyhow::anyhow!("failed to send tick: drain channel closed"));
                }
            }
        }

        let Some(builder) = builders.get_mut(product_code) else {
            continue;
        };

        let from_tick = builder.on_tick(price, size, ts, best_bid, best_ask);
        let from_complete = builder.try_complete(ts, best_bid, best_ask);
        if let Some(candle) = from_tick.or(from_complete) {
            if let Some(pool) = pool
                && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &candle).await
            {
                tracing::warn!(error = %e, "failed to save crypto candle");
            }
            let pair_key = candle.pair.0.clone();
            let (event, indicators) =
                emit_candle_event(candle, closes_map, highs_map, lows_map, true);
            latest_indicators.insert(pair_key, indicators);
            if price_tx.send(event).await.is_err() {
                tracing::info!("price channel closed, stopping bitflyer monitor");
                return Ok(());
            }
        }

        if let Some(h1_builder) = h1_builders.get_mut(product_code) {
            let h1_from_tick = h1_builder.on_tick(price, size, ts, best_bid, best_ask);
            let h1_from_complete = h1_builder.try_complete(ts, best_bid, best_ask);
            if let Some(h1_candle) = h1_from_tick.or(h1_from_complete) {
                if let Some(pool) = pool
                    && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &h1_candle).await
                {
                    tracing::warn!(error = %e, "failed to save h1 crypto candle");
                }
                let (mut h1_event, _h1_indicators) =
                    emit_candle_event(h1_candle, closes_map_h1, highs_map_h1, lows_map_h1, true);
                if let Some(m5) = latest_indicators.get(product_code) {
                    for (key, value) in m5 {
                        h1_event
                            .indicators
                            .insert(format!("{primary_tf_prefix}_{key}"), *value);
                    }
                }
                if price_tx.send(h1_event).await.is_err() {
                    tracing::info!("price channel closed, stopping bitflyer monitor");
                    return Ok(());
                }
            }
        }
    }
}

#[async_trait]
impl MarketFeed for BitflyerMonitor {
    async fn run(
        self: Box<Self>,
        price_store: Arc<PriceStore>,
        price_tx: mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()> {
        (*self).run_inner(price_store, price_tx).await
    }
}

#[cfg(test)]
mod tests;
