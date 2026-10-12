const PRICE_STORE_TICK_CHANNEL_CAP: usize = 1024;

mod candles;
mod reconnect;
mod stream;
mod types;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::mpsc;

use crate::candle_builder::build_primary_and_h1_builders;
use crate::market_feed::MarketFeed;
use crate::price_store::PriceStore;

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
        let (mut builders, mut h1_builders) = build_primary_and_h1_builders(
            self.pairs.iter().filter(|p| !is_spot_only(p)),
            Exchange::BitflyerCfd,
            &self.timeframe,
        );

        let mut closes_map: HashMap<String, Vec<Decimal>> = std::mem::take(&mut self.closes_seed);
        let mut highs_map: HashMap<String, Vec<Decimal>> = std::mem::take(&mut self.highs_seed);
        let mut lows_map: HashMap<String, Vec<Decimal>> = std::mem::take(&mut self.lows_seed);
        let mut closes_map_h1: HashMap<String, Vec<Decimal>> = HashMap::new();
        let mut highs_map_h1: HashMap<String, Vec<Decimal>> = HashMap::new();
        let mut lows_map_h1: HashMap<String, Vec<Decimal>> = HashMap::new();
        candles::log_seeded_warmup_closes(&closes_map, &self.timeframe);

        let tick_tx = candles::spawn_tick_drain(price_store);

        let mut backoff_secs = 1u64;

        loop {
            let result = stream::connect_and_stream(
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
            .await;

            if let Some(outcome) =
                reconnect::handle_stream_result(result, &price_tx, &tick_tx, &mut backoff_secs)
                    .await
            {
                return outcome;
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
