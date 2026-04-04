use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::Pair;
use crate::indicators;
use crate::oanda::OandaClient;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

pub struct MarketMonitor {
    client: OandaClient,
    pairs: Vec<Pair>,
    interval_secs: u64,
    tx: mpsc::Sender<PriceEvent>,
}

impl MarketMonitor {
    pub fn new(
        client: OandaClient,
        pairs: Vec<Pair>,
        interval_secs: u64,
        tx: mpsc::Sender<PriceEvent>,
    ) -> Self {
        Self { client, pairs, interval_secs, tx }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let mut tick = interval(Duration::from_secs(self.interval_secs));
        loop {
            tick.tick().await;
            for pair in &self.pairs {
                match self.fetch_and_emit(pair).await {
                    Ok(()) => {}
                    Err(e) => tracing::error!("monitor error for {pair}: {e}"),
                }
            }
        }
    }

    async fn fetch_and_emit(&self, pair: &Pair) -> anyhow::Result<()> {
        let candles = self.client.get_candles(pair, "M5", 100).await?;
        if candles.is_empty() {
            return Ok(());
        }
        let latest = candles.last().unwrap().clone();
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
            candle: latest,
            indicators,
            timestamp: chrono::Utc::now(),
        };
        self.tx.send(event).await?;
        Ok(())
    }
}
