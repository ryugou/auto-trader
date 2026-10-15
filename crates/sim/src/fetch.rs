use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::data;
use crate::error::SimError;
use crate::types::{
    Bar, GMO_HTTP_TIMEOUT, GMO_INTERVAL, GMO_KLINE_PATH, GMO_MIN_REQUEST_INTERVAL,
    GMO_RETRY_DELAYS, GMO_SYMBOL,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceType {
    Bid,
    Ask,
}

impl PriceType {
    fn as_query_value(self) -> &'static str {
        match self {
            PriceType::Bid => "BID",
            PriceType::Ask => "ASK",
        }
    }
}

impl std::fmt::Display for PriceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_query_value())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kline {
    pub open_time: i64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
}

#[derive(Debug, serde::Deserialize)]
struct KlineResponse {
    status: i32,
    #[serde(default)]
    data: Vec<KlineRaw>,
}

#[derive(Debug, serde::Deserialize)]
struct KlineRaw {
    #[serde(rename = "openTime")]
    open_time: String,
    open: String,
    high: String,
    low: String,
    close: String,
}

pub struct GmoKlineClient {
    base_url: String,
    http: reqwest::Client,
    min_interval: Duration,
    retry_delays: Vec<Duration>,
    next_request_at: Mutex<Option<Instant>>,
}

impl GmoKlineClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(GMO_HTTP_TIMEOUT)
                .build()
                .expect("reqwest client with a fixed timeout and no other options must build"),
            min_interval: GMO_MIN_REQUEST_INTERVAL,
            retry_delays: GMO_RETRY_DELAYS.to_vec(),
            next_request_at: Mutex::new(None),
        }
    }

    pub fn with_timing(mut self, min_interval: Duration, retry_delays: Vec<Duration>) -> Self {
        self.min_interval = min_interval;
        self.retry_delays = retry_delays;
        self
    }

    async fn throttle(&self) {
        let wait = {
            let mut guard = self
                .next_request_at
                .lock()
                .expect("next_request_at mutex poisoned by a panicked holder");
            let now = Instant::now();
            let earliest = guard.unwrap_or(now);
            let start_at = earliest.max(now);
            *guard = Some(start_at + self.min_interval);
            start_at.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    async fn try_fetch(&self, url: &str) -> Result<KlineResponse, String> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("failed to send gmo kline request: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!(
                "failed to fetch gmo kline: unexpected http status {status}"
            ));
        }
        let body: KlineResponse = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse gmo kline response body: {e}"))?;
        if body.status != 0 {
            return Err(format!(
                "failed to fetch gmo kline: unexpected api status {}",
                body.status
            ));
        }
        Ok(body)
    }

    pub async fn fetch_day(
        &self,
        date: NaiveDate,
        price_type: PriceType,
    ) -> Result<Vec<Kline>, String> {
        let date_str = date.format("%Y%m%d").to_string();
        let url = format!(
            "{}/{}?symbol={}&priceType={}&interval={}&date={}",
            self.base_url, GMO_KLINE_PATH, GMO_SYMBOL, price_type, GMO_INTERVAL, date_str
        );

        let attempts = self.retry_delays.len() + 1;
        let mut last_err = String::new();
        for attempt in 0..attempts {
            if attempt > 0 {
                tokio::time::sleep(self.retry_delays[attempt - 1]).await;
            }
            self.throttle().await;
            match self.try_fetch(&url).await {
                Ok(body) => return Ok(parse_klines(&body.data, &date_str, price_type)),
                Err(e) => {
                    tracing::warn!(
                        date = %date_str,
                        price_type = %price_type,
                        attempt = attempt + 1,
                        max_attempts = attempts,
                        error = %e,
                        "failed to fetch gmo kline: attempt failed"
                    );
                    last_err = e;
                }
            }
        }
        let message = format!(
            "failed to fetch gmo kline for date={date_str} priceType={price_type} after {attempts} attempts: {last_err}"
        );
        tracing::error!(
            date = %date_str,
            price_type = %price_type,
            attempts,
            error = %last_err,
            "failed to fetch gmo kline: exhausted all retry attempts"
        );
        Err(message)
    }
}

fn parse_klines(raw: &[KlineRaw], date_str: &str, price_type: PriceType) -> Vec<Kline> {
    let mut out = Vec::with_capacity(raw.len());
    let mut skipped = 0usize;
    for k in raw {
        match parse_kline(k) {
            Some(kline) => out.push(kline),
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            date = %date_str,
            price_type = %price_type,
            skipped,
            total = raw.len(),
            "invalid gmo kline entries: not parseable into integer milli-yen prices; skipped"
        );
    }
    out
}

fn parse_kline(raw: &KlineRaw) -> Option<Kline> {
    let open_time_ms: i64 = raw.open_time.parse().ok()?;
    if open_time_ms < 0 || open_time_ms % 1000 != 0 {
        return None;
    }
    Some(Kline {
        open_time: open_time_ms / 1000,
        open: decimal_str_to_milli(&raw.open)?,
        high: decimal_str_to_milli(&raw.high)?,
        low: decimal_str_to_milli(&raw.low)?,
        close: decimal_str_to_milli(&raw.close)?,
    })
}

fn decimal_str_to_milli(s: &str) -> Option<i64> {
    let d = Decimal::from_str(s).ok()?;
    data::decimal_to_milli(d).ok()
}

#[derive(Debug)]
pub struct JoinOutcome {
    pub bars: Vec<Bar>,
    pub one_sided: usize,
    pub invalid: usize,
}

pub fn join_and_validate(bid: &[Kline], ask: &[Kline]) -> JoinOutcome {
    use std::collections::BTreeMap;

    fn index_by_open_time(klines: &[Kline]) -> BTreeMap<i64, Option<&Kline>> {
        let mut map: BTreeMap<i64, Option<&Kline>> = BTreeMap::new();
        for k in klines {
            map.entry(k.open_time)
                .and_modify(|slot| *slot = None)
                .or_insert(Some(k));
        }
        map
    }

    let bid_map = index_by_open_time(bid);
    let ask_map = index_by_open_time(ask);

    let mut times: Vec<i64> = bid_map.keys().chain(ask_map.keys()).copied().collect();
    times.sort_unstable();
    times.dedup();

    let mut bars = Vec::new();
    let mut one_sided = 0usize;
    let mut invalid = 0usize;

    for t in times {
        let b = bid_map.get(&t).copied();
        let a = ask_map.get(&t).copied();
        match (b, a) {
            (Some(None), _) | (_, Some(None)) => invalid += 1,
            (Some(Some(b)), Some(Some(a))) => {
                if is_valid_pair(b, a) {
                    bars.push(Bar {
                        open_time: t,
                        bid_open: b.open,
                        bid_high: b.high,
                        bid_low: b.low,
                        bid_close: b.close,
                        ask_open: a.open,
                        ask_high: a.high,
                        ask_low: a.low,
                        ask_close: a.close,
                    });
                } else {
                    invalid += 1;
                }
            }
            _ => one_sided += 1,
        }
    }

    JoinOutcome {
        bars,
        one_sided,
        invalid,
    }
}

fn is_valid_pair(bid: &Kline, ask: &Kline) -> bool {
    for side in [bid, ask] {
        if side.open <= 0 || side.high <= 0 || side.low <= 0 || side.close <= 0 {
            return false;
        }
        if side.high < side.open.max(side.close) || side.low > side.open.min(side.close) {
            return false;
        }
    }
    if ask.open < bid.open || ask.high < bid.high || ask.low < bid.low || ask.close < bid.close {
        return false;
    }
    true
}

#[derive(Debug)]
pub struct BackfillReport {
    pub days: usize,
    pub saved: u64,
    pub one_sided: usize,
    pub invalid: usize,
    pub failed: Vec<String>,
}

pub fn gmo_kline_min_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2023, 10, 28).expect("2023-10-28 is a valid calendar date")
}

pub async fn backfill(
    pool: &sqlx::PgPool,
    client: &GmoKlineClient,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<BackfillReport, SimError> {
    let mut report = BackfillReport {
        days: 0,
        saved: 0,
        one_sided: 0,
        invalid: 0,
        failed: Vec::new(),
    };

    let mut date = from;
    while date <= to {
        report.days += 1;
        let date_str = date.format("%Y%m%d").to_string();

        let bid_result = client.fetch_day(date, PriceType::Bid).await;
        let ask_result = client.fetch_day(date, PriceType::Ask).await;

        match (bid_result, ask_result) {
            (Ok(bid), Ok(ask)) => {
                let outcome = join_and_validate(&bid, &ask);
                if outcome.one_sided > 0 || outcome.invalid > 0 {
                    tracing::warn!(
                        date = %date_str,
                        price_type = "BID+ASK",
                        bid_count = bid.len(),
                        ask_count = ask.len(),
                        kept = outcome.bars.len(),
                        one_sided = outcome.one_sided,
                        invalid = outcome.invalid,
                        "invalid bars for date: one-sided or invalid pricing data; dropped"
                    );
                }
                report.one_sided += outcome.one_sided;
                report.invalid += outcome.invalid;
                if !outcome.bars.is_empty() {
                    let bar_count = outcome.bars.len();
                    match data::upsert_bars(pool, &outcome.bars).await {
                        Ok(affected) => report.saved += affected,
                        Err(e) => {
                            tracing::error!(
                                date = %date_str,
                                bar_count,
                                error = %e,
                                "failed to save bars to sim_candles"
                            );
                            return Err(e);
                        }
                    }
                }
            }
            (bid_result, ask_result) => {
                if let Err(e) = bid_result {
                    tracing::error!(
                        date = %date_str,
                        price_type = "BID",
                        error = %e,
                        "failed to fetch gmo kline for date: skipping this date"
                    );
                    report.failed.push(format!("{date_str} BID"));
                }
                if let Err(e) = ask_result {
                    tracing::error!(
                        date = %date_str,
                        price_type = "ASK",
                        error = %e,
                        "failed to fetch gmo kline for date: skipping this date"
                    );
                    report.failed.push(format!("{date_str} ASK"));
                }
            }
        }

        if date == to {
            break;
        }
        date = match date.succ_opt() {
            Some(next) => next,
            None => break,
        };
    }

    if report.failed.is_empty() {
        Ok(report)
    } else {
        Err(SimError::FetchIncomplete(report.failed))
    }
}

#[cfg(test)]
mod tests;
