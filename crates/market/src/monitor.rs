use std::sync::Arc;

use async_trait::async_trait;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

use crate::indicators;
use crate::market_feed::MarketFeed;
use crate::oanda::OandaClient;
use crate::price_store::PriceStore;

pub struct MarketMonitor {
    client: OandaClient,
    pairs: Vec<Pair>,
    interval_secs: u64,
    timeframe: String,
    pool: Option<PgPool>,
}

impl MarketMonitor {
    pub fn new(client: OandaClient, pairs: Vec<Pair>, interval_secs: u64, timeframe: &str) -> Self {
        Self {
            client,
            pairs,
            interval_secs,
            timeframe: timeframe.to_string(),
            pool: None,
        }
    }

    pub fn with_db(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    async fn fetch_and_emit(
        &self,
        pair: &Pair,
        price_tx: &mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()> {
        let candles = self.client.get_candles(pair, &self.timeframe, 100).await?;
        let latest = match candles.last() {
            Some(c) => c.clone(),
            None => return Ok(()),
        };

        if let Some(pool) = &self.pool {
            for candle in &candles {
                if let Err(e) = auto_trader_db::candles::upsert_candle(pool, candle).await {
                    tracing::warn!(error = %e, "failed to save candle");
                }
            }
        }
        let closes: Vec<Decimal> = candles.iter().map(|c| c.close).collect();
        let highs: Vec<Decimal> = candles.iter().map(|c| c.high).collect();
        let lows: Vec<Decimal> = candles.iter().map(|c| c.low).collect();

        let mut indicators = indicators::indicator_map(&closes, &highs, &lows);
        if let Some(v) = indicators::ema(&closes, 20) {
            indicators.insert("ema_20".to_string(), v);
        }

        let event = PriceEvent {
            pair: pair.clone(),
            exchange: Exchange::Oanda,
            timestamp: latest.timestamp,
            candle: latest,
            indicators,
        };
        price_tx.send(event).await?;
        Ok(())
    }
}

#[async_trait]
impl MarketFeed for MarketMonitor {
    async fn run(
        self: Box<Self>,
        _price_store: Arc<PriceStore>,
        price_tx: mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()> {
        (*self).run_inner(_price_store, price_tx).await
    }
}

impl MarketMonitor {
    async fn run_inner(
        self,
        _price_store: Arc<PriceStore>,
        price_tx: mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()> {
        let mut tick = interval(Duration::from_secs(self.interval_secs));
        loop {
            tick.tick().await;
            for pair in &self.pairs {
                match self.fetch_and_emit(pair, &price_tx).await {
                    Ok(()) => {}
                    Err(e) => {
                        if price_tx.is_closed() {
                            tracing::info!("price channel closed, stopping monitor");
                            return Ok(());
                        }
                        tracing::error!(pair = %pair, error = %e, "failed to fetch and emit candle");
                    }
                }
            }
        }
    }
}
