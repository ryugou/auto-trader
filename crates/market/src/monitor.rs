use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use crate::indicators;
use crate::oanda::OandaClient;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

pub struct MarketMonitor {
    client: OandaClient,
    pairs: Vec<Pair>,
    interval_secs: u64,
    timeframe: String,
    tx: mpsc::Sender<PriceEvent>,
    pool: Option<PgPool>,
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
            client,
            pairs,
            interval_secs,
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
        let highs: Vec<Decimal> = candles.iter().map(|c| c.high).collect();
        let lows: Vec<Decimal> = candles.iter().map(|c| c.low).collect();

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
        if let Some(v) = indicators::atr(&highs, &lows, &closes, 14) {
            indicators.insert("atr_14".to_string(), v);
        }
        if let Some(v) = indicators::adx(&highs, &lows, &closes, 14) {
            indicators.insert("adx_14".to_string(), v);
        }
        if let Some((bb_lo, bb_mid, bb_up)) =
            indicators::bollinger_bands(&closes, 20, Decimal::from(2))
            && bb_mid > Decimal::ZERO
        {
            let bb_width_pct = (bb_up - bb_lo) / bb_mid * Decimal::from(100);
            indicators.insert("bb_width_pct".to_string(), bb_width_pct);
        }
        // ATR percentile within the available candle window
        if let Some(current_atr) = indicators.get("atr_14").copied() {
            let lookback = 50.min(closes.len());
            if lookback >= 15 {
                let mut atr_count_below = 0u32;
                let mut atr_total = 0u32;
                for end in (closes.len() - lookback)..closes.len() {
                    if end >= 14
                        && let Some(past_atr) = indicators::atr(
                            &highs[..=end],
                            &lows[..=end],
                            &closes[..=end],
                            14,
                        )
                    {
                        atr_total += 1;
                        if past_atr < current_atr {
                            atr_count_below += 1;
                        }
                    }
                }
                if atr_total > 0 {
                    let pct = Decimal::from(atr_count_below)
                        / Decimal::from(atr_total)
                        * Decimal::from(100);
                    indicators.insert("atr_percentile".to_string(), pct);
                }
            }
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
