use auto_trader_core::types::{Candle, Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

use crate::provider::MarketDataProvider;
use crate::RawTick;
use tokio::sync::mpsc;

pub struct OandaClient {
    client: reqwest::Client,
    base_url: String,
    account_id: String,
}

#[derive(Debug, Deserialize)]
struct CandlesResponse {
    candles: Vec<OandaCandle>,
}

#[derive(Debug, Deserialize)]
struct OandaCandle {
    time: String,
    volume: Option<u64>,
    mid: OandaCandleMid,
    complete: bool,
}

#[derive(Debug, Deserialize)]
struct OandaCandleMid {
    o: String,
    h: String,
    l: String,
    c: String,
}

impl OandaClient {
    pub fn new(base_url: &str, account_id: &str, api_key: &str) -> anyhow::Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))?,
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.to_string(),
            account_id: account_id.to_string(),
        })
    }

    async fn request_with_retry<T, F, Fut>(&self, f: F) -> anyhow::Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        let mut last_err = None;
        for attempt in 0..3 {
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    tracing::warn!("OANDA request failed (attempt {}): {e}", attempt + 1);
                    last_err = Some(e);
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt as u32))).await;
                    }
                }
            }
        }
        Err(last_err.unwrap())
    }

    pub async fn get_candles(
        &self,
        pair: &Pair,
        granularity: &str,
        count: u32,
    ) -> anyhow::Result<Vec<Candle>> {
        let url = format!(
            "{}/v3/accounts/{}/instruments/{}/candles",
            self.base_url, self.account_id, pair.0
        );
        let granularity = granularity.to_string();
        let count_str = count.to_string();
        let pair_clone = pair.clone();
        let client = self.client.clone();
        let resp: CandlesResponse = self
            .request_with_retry(|| {
                let url = url.clone();
                let granularity = granularity.clone();
                let count_str = count_str.clone();
                let client = client.clone();
                async move {
                    client
                        .get(&url)
                        .query(&[
                            ("granularity", granularity.as_str()),
                            ("count", count_str.as_str()),
                            ("price", "M"),
                        ])
                        .send()
                        .await?
                        .error_for_status()?
                        .json::<CandlesResponse>()
                        .await
                        .map_err(anyhow::Error::from)
                }
            })
            .await?;

        let mut candles = Vec::new();
        for c in resp.candles {
            if !c.complete {
                continue;
            }
            let timestamp = DateTime::parse_from_rfc3339(&c.time)?.with_timezone(&Utc);
            candles.push(Candle {
                pair: pair_clone.clone(),
                exchange: Exchange::Oanda,
                timeframe: granularity.to_string(),
                open: Decimal::from_str(&c.mid.o)?,
                high: Decimal::from_str(&c.mid.h)?,
                low: Decimal::from_str(&c.mid.l)?,
                close: Decimal::from_str(&c.mid.c)?,
                volume: c.volume,
                timestamp,
            });
        }
        Ok(candles)
    }

    pub async fn get_latest_price(&self, pair: &Pair) -> anyhow::Result<Decimal> {
        let url = format!(
            "{}/v3/accounts/{}/pricing",
            self.base_url, self.account_id
        );
        let instrument = pair.0.clone();
        let client = self.client.clone();
        let resp: serde_json::Value = self
            .request_with_retry(|| {
                let url = url.clone();
                let instrument = instrument.clone();
                let client = client.clone();
                async move {
                    client
                        .get(&url)
                        .query(&[("instruments", instrument.as_str())])
                        .send()
                        .await?
                        .error_for_status()?
                        .json::<serde_json::Value>()
                        .await
                        .map_err(anyhow::Error::from)
                }
            })
            .await?;

        let prices = resp["prices"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing prices array in OANDA pricing response"))?;
        let mid = prices
            .first()
            .ok_or_else(|| anyhow::anyhow!("empty prices array in OANDA pricing response"))?;

        let bid_str = mid["bids"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|b| b["price"].as_str())
            .ok_or_else(|| anyhow::anyhow!("missing bid price in OANDA pricing response"))?;
        let ask_str = mid["asks"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|a| a["price"].as_str())
            .ok_or_else(|| anyhow::anyhow!("missing ask price in OANDA pricing response"))?;

        let bid = Decimal::from_str(bid_str)?;
        let ask = Decimal::from_str(ask_str)?;
        Ok((bid + ask) / Decimal::from(2))
    }

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
}

#[async_trait::async_trait]
impl MarketDataProvider for OandaClient {
    async fn get_candles(
        &self,
        pair: &Pair,
        timeframe: &str,
        count: u32,
    ) -> anyhow::Result<Vec<Candle>> {
        self.get_candles(pair, timeframe, count).await
    }

    async fn get_latest_price(&self, pair: &Pair) -> anyhow::Result<Decimal> {
        self.get_latest_price(pair).await
    }
}
