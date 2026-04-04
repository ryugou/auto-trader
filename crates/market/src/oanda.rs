use auto_trader_core::types::{Candle, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

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
    volume: Option<i32>,
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
    pub fn new(base_url: &str, account_id: &str, api_key: &str) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")).unwrap(),
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        Self {
            client,
            base_url: base_url.to_string(),
            account_id: account_id.to_string(),
        }
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
        let resp: CandlesResponse = self
            .client
            .get(&url)
            .query(&[
                ("granularity", granularity),
                ("count", &count.to_string()),
                ("price", "M"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut candles = Vec::new();
        for c in resp.candles {
            if !c.complete {
                continue;
            }
            let timestamp = DateTime::parse_from_rfc3339(&c.time)?.with_timezone(&Utc);
            candles.push(Candle {
                pair: pair.clone(),
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
        let resp: serde_json::Value = self
            .client
            .get(&url)
            .query(&[("instruments", pair.0.as_str())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let prices = resp["prices"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("no prices in response"))?;
        let mid = &prices[0];
        let bid = Decimal::from_str(mid["bids"][0]["price"].as_str().unwrap_or("0"))?;
        let ask = Decimal::from_str(mid["asks"][0]["price"].as_str().unwrap_or("0"))?;
        Ok((bid + ask) / Decimal::from(2))
    }
}
