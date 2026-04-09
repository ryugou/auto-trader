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
