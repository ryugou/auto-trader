use auto_trader_core::types::{Candle, Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

use crate::provider::MarketDataProvider;

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
                        tokio::time::sleep(std::time::Duration::from_secs(
                            2u64.pow(attempt as u32),
                        ))
                        .await;
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
        let url = format!("{}/v3/accounts/{}/pricing", self.base_url, self.account_id);
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
