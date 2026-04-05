use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use crate::candle_builder::CandleBuilder;
use crate::indicators;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
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
    #[allow(dead_code)]
    best_bid: Decimal,
    #[allow(dead_code)]
    best_ask: Decimal,
    ltp: Decimal,
    volume: Decimal,
    timestamp: String,
}

pub struct BitflyerMonitor {
    ws_url: String,
    pairs: Vec<Pair>,
    timeframe: String,
    tx: mpsc::Sender<PriceEvent>,
    pool: Option<PgPool>,
}

impl BitflyerMonitor {
    pub fn new(
        ws_url: &str,
        pairs: Vec<Pair>,
        timeframe: &str,
        tx: mpsc::Sender<PriceEvent>,
    ) -> Self {
        Self {
            ws_url: ws_url.to_string(),
            pairs,
            timeframe: timeframe.to_string(),
            tx,
            pool: None,
        }
    }

    pub fn with_db(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let mut builders: HashMap<String, CandleBuilder> = HashMap::new();
        for pair in &self.pairs {
            builders.insert(
                pair.0.clone(),
                CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, self.timeframe.clone()),
            );
        }
        let mut closes_map: HashMap<String, Vec<Decimal>> = HashMap::new();
        let mut backoff_secs = 1u64;

        loop {
            match self.connect_and_stream(&mut builders, &mut closes_map).await {
                Ok(()) => {
                    if self.tx.is_closed() {
                        tracing::info!("price channel closed, stopping bitflyer monitor");
                        return Ok(());
                    }
                    // Normal close — reconnect (server can close gracefully)
                    tracing::info!("bitflyer websocket closed normally, reconnecting");
                    backoff_secs = 1;
                    continue;
                }
                Err(e) => {
                    if self.tx.is_closed() {
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

    async fn connect_and_stream(
        &self,
        builders: &mut HashMap<String, CandleBuilder>,
        closes_map: &mut HashMap<String, Vec<Decimal>>,
    ) -> anyhow::Result<()> {
        let (ws, _) = connect_async(&self.ws_url).await?;
        let (mut write, mut read) = ws.split();
        tracing::info!("bitflyer websocket connected: {}", self.ws_url);

        for pair in &self.pairs {
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
            let ts = chrono::DateTime::parse_from_rfc3339(&ticker.timestamp)?
                .with_timezone(&chrono::Utc);

            // on_tick returns completed candle when period boundary is crossed
            let from_tick = builder.on_tick(price, size, ts);
            let from_complete = builder.try_complete(ts);
            let completed = from_tick.or(from_complete);

            if let Some(candle) = completed {
                if let Some(pool) = &self.pool
                    && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &candle).await
                {
                    tracing::warn!("failed to save crypto candle: {e}");
                }

                let closes = closes_map.entry(product_code.clone()).or_default();
                closes.push(candle.close);
                if closes.len() > 200 {
                    closes.drain(..closes.len() - 200);
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

                let event = PriceEvent {
                    pair: candle.pair.clone(),
                    exchange: Exchange::BitflyerCfd,
                    timestamp: candle.timestamp,
                    candle,
                    indicators: indicator_map,
                };

                if self.tx.send(event).await.is_err() {
                    tracing::info!("price channel closed, stopping bitflyer monitor");
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
