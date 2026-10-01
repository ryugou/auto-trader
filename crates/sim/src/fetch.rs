//! GMO コイン外国為替 FX 公開 API から USD/JPY の M5 足を取得し、買値・売値を
//! 結合・検証して `sim_candles` へ保存する(spec 5.2 章)。
//!
//! この基盤が扱うのは USD/JPY の M5 足だけ(spec 5.1 章)であり、シンボル・
//! 足種は固定する。価格はミリ円(円 × 1000)の `i64` で表す(spec 3 章)。

use crate::data;
use crate::error::SimError;
use crate::types::{
    Bar, GMO_HTTP_TIMEOUT, GMO_INTERVAL, GMO_KLINE_PATH, GMO_MIN_REQUEST_INTERVAL,
    GMO_RETRY_DELAYS, GMO_SYMBOL,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

/// GMO KLine API の 1 本の足(秒・ミリ円)。
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

/// GMO 公開 API の KLine エンドポイント専用クライアント。
pub struct GmoKlineClient {
    base_url: String,
    http: reqwest::Client,
    min_interval: Duration,
    retry_delays: Vec<Duration>,
    /// 直近のリクエスト発行予定時刻。複数回の `fetch_day` 呼び出し(BID/ASK の
    /// 2 回、複数日)をまたいで spec 5.2 章の「リクエストの間隔は 1 秒以上
    /// あける」を守るための共有状態。
    next_request_at: Mutex<Option<Instant>>,
}

impl GmoKlineClient {
    /// `min_interval = 1 秒`、`retry_delays = [2s, 4s, 8s]`(spec 5.2 章)。
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

    /// テスト用に間隔・再試行間隔を上書きする。
    pub fn with_timing(mut self, min_interval: Duration, retry_delays: Vec<Duration>) -> Self {
        self.min_interval = min_interval;
        self.retry_delays = retry_delays;
        self
    }

    /// 直前のリクエストから `min_interval` 未満しか経っていなければ待つ。
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

    /// 1 回の HTTP リクエスト・JSON 解析・`status` チェックを行う。
    /// ネットワーク障害・非 2xx・JSON 不正・API `status != 0` は、すべて
    /// 再試行対象の失敗として `Err` にする(spec 5.2 章)。
    async fn try_fetch(&self, url: &str) -> Result<KlineResponse, String> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("http request failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("http status {status}"));
        }
        let body: KlineResponse = resp
            .json()
            .await
            .map_err(|e| format!("response body parse failed: {e}"))?;
        if body.status != 0 {
            return Err(format!("api status {}", body.status));
        }
        Ok(body)
    }

    /// 指定した日付・`priceType` の M5 足を取得する(spec 5.2 章)。
    /// 失敗が続く場合は `retry_delays` の間隔で再試行し、すべて失敗したら
    /// `Err` に日付・`priceType`・最後の失敗理由を含めて返す。
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
                        "gmo kline fetch attempt failed"
                    );
                    last_err = e;
                }
            }
        }
        let message = format!(
            "gmo kline fetch failed for date={date_str} priceType={price_type} after {attempts} attempts: {last_err}"
        );
        tracing::error!(date = %date_str, price_type = %price_type, attempts, "{message}");
        Err(message)
    }
}

/// レスポンスの `data` 配列を `Kline` へ変換する。価格文字列を 1000 倍して
/// 整数にならない要素は不正な足として扱い(spec Task 2 Interfaces)、保存対象
/// から除外してその件数を WARN で記録する。5.2 章の OHLC 整合性チェックとは
/// 独立した、レスポンス自体の型異常に対する防御である。
///
/// 解析不能で除外した要素の相手側(同じ `openTime` のもう一方の `priceType`)は、
/// `join_and_validate` で片側だけの足として `one_sided` に数えられる。
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
            "gmo kline response contained entries that do not parse into integer milli-yen prices; skipped"
        );
    }
    out
}

fn parse_kline(raw: &KlineRaw) -> Option<Kline> {
    let open_time_ms: i64 = raw.open_time.parse().ok()?;
    // 1000 で割り切れない値(ミリ秒の端数)と負の値は、黙って丸めずに不正な足とする。
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

/// 価格文字列を `rust_decimal` で読み、1000 倍して整数にならない値は
/// `None`(不正な足)として扱う(spec Task 2 Interfaces)。
fn decimal_str_to_milli(s: &str) -> Option<i64> {
    let d = Decimal::from_str(s).ok()?;
    let milli = d * Decimal::from(1000);
    if !milli.fract().is_zero() {
        return None;
    }
    milli.to_i64()
}

/// `join_and_validate` の結果。
#[derive(Debug)]
pub struct JoinOutcome {
    pub bars: Vec<Bar>,
    pub one_sided: usize,
    pub invalid: usize,
}

/// BID/ASK の `Kline` 列を `open_time` で結合し、spec 5.2 章の妥当性を検証する。
/// 戻り値の `bars` は `open_time` 昇順。
///
/// BID/ASK のどちらかで同じ `open_time` が 2 回以上現れた場合、どちらの値が
/// 正しいか判断できないため、その `open_time` の足は保存せず、`invalid` に
/// `open_time` 1 つにつき 1 件として数える(もう一方の側に足が無くても
/// `one_sided` ではなく `invalid`)。
pub fn join_and_validate(bid: &[Kline], ask: &[Kline]) -> JoinOutcome {
    use std::collections::BTreeMap;

    /// 値が `None` の `open_time` は同じ側で重複していたことを表す。
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

/// spec 5.2 章の妥当性チェック。買値・売値それぞれの OHLC 整合性と、
/// 買値に対する売値の大小関係を検証する。
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

/// `backfill` の結果の要約。`failed` は `"YYYYMMDD BID"` / `"YYYYMMDD ASK"` の形。
#[derive(Debug)]
pub struct BackfillReport {
    pub days: usize,
    pub saved: u64,
    pub one_sided: usize,
    pub invalid: usize,
    pub failed: Vec<String>,
}

/// `from` から `to` までの各日付(両端を含む)について BID/ASK を取得し、
/// 結合・検証して `sim_candles` へ保存する(spec 5.2 章)。
///
/// 1 日・1 priceType の取得失敗では処理を止めず、ERROR を記録して次へ進む。
/// 失敗が 1 件でもあれば、全日付の処理を終えた後に
/// `SimError::FetchIncomplete` で失敗一覧を返す(同じ範囲を再実行すれば
/// 欠けた日付が埋まる設計のため、成功した日は保存済みのまま残す)。
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
                        "gmo kline backfill: dropped bars for date (one-sided or failed 5.2 validation)"
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
                                "gmo kline backfill: saving bars to sim_candles failed"
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
                        "gmo kline backfill: BID fetch failed for date, skipping this date"
                    );
                    report.failed.push(format!("{date_str} BID"));
                }
                if let Err(e) = ask_result {
                    tracing::error!(
                        date = %date_str,
                        price_type = "ASK",
                        error = %e,
                        "gmo kline backfill: ASK fetch failed for date, skipping this date"
                    );
                    report.failed.push(format!("{date_str} ASK"));
                }
            }
        }

        // `to` を処理し終えたら抜ける。`succ_opt()` を呼ばないので、
        // `to == NaiveDate::MAX` でもオーバーフローしない。
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
mod tests {
    use super::*;

    /// 全フィールドが同値のフラットな `Kline`(OHLC 整合性を自明に満たす)。
    fn flat(open_time: i64, price: i64) -> Kline {
        Kline {
            open_time,
            open: price,
            high: price,
            low: price,
            close: price,
        }
    }

    #[test]
    fn matching_open_time_joins_into_one_bar() {
        let bid = vec![flat(1_000, 149_600)];
        let ask = vec![flat(1_000, 149_610)];

        let outcome = join_and_validate(&bid, &ask);

        assert_eq!(outcome.bars.len(), 1);
        assert_eq!(outcome.one_sided, 0);
        assert_eq!(outcome.invalid, 0);
        assert_eq!(
            outcome.bars[0],
            Bar {
                open_time: 1_000,
                bid_open: 149_600,
                bid_high: 149_600,
                bid_low: 149_600,
                bid_close: 149_600,
                ask_open: 149_610,
                ask_high: 149_610,
                ask_low: 149_610,
                ask_close: 149_610,
            }
        );
    }

    #[test]
    fn duplicate_open_time_on_bid_side_is_invalid_and_not_saved() {
        let bid = vec![
            flat(1_000, 149_600),
            flat(1_000, 149_601),
            flat(2_000, 149_605),
        ];
        let ask = vec![flat(1_000, 149_610), flat(2_000, 149_615)];

        let outcome = join_and_validate(&bid, &ask);

        assert_eq!(outcome.bars.len(), 1);
        assert_eq!(outcome.bars[0].open_time, 2_000);
        assert_eq!(outcome.invalid, 1);
        assert_eq!(outcome.one_sided, 0);
    }

    #[test]
    fn duplicate_open_time_on_ask_side_only_is_invalid_not_one_sided() {
        let bid: Vec<Kline> = vec![];
        let ask = vec![flat(1_000, 149_610), flat(1_000, 149_611)];

        let outcome = join_and_validate(&bid, &ask);

        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.invalid, 1);
        assert_eq!(outcome.one_sided, 0);
    }

    #[test]
    fn bid_only_and_ask_only_bars_are_counted_one_sided_and_excluded() {
        // 1000: 両方あり -> bar。2000: bid だけ。3000: ask だけ。
        let bid = vec![flat(1_000, 149_600), flat(2_000, 149_605)];
        let ask = vec![flat(1_000, 149_610), flat(3_000, 149_615)];

        let outcome = join_and_validate(&bid, &ask);

        assert_eq!(outcome.bars.len(), 1);
        assert_eq!(outcome.one_sided, 2, "2000 (bid-only) + 3000 (ask-only)");
        assert_eq!(outcome.invalid, 0);
    }

    #[test]
    fn zero_price_is_invalid() {
        let bid = vec![Kline {
            open_time: 1_000,
            open: 0,
            high: 149_600,
            low: 149_600,
            close: 149_600,
        }];
        let ask = vec![flat(1_000, 149_610)];

        let outcome = join_and_validate(&bid, &ask);

        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.invalid, 1);
        assert_eq!(outcome.one_sided, 0);
    }

    #[test]
    fn negative_price_is_invalid() {
        let bid = vec![flat(1_000, 149_600)];
        let ask = vec![Kline {
            open_time: 1_000,
            open: 149_610,
            high: 149_610,
            low: 149_610,
            close: -10,
        }];

        let outcome = join_and_validate(&bid, &ask);

        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.invalid, 1);
    }

    #[test]
    fn high_below_max_of_open_and_close_is_invalid() {
        // max(open, close) = 149_650, high = 149_605 < 149_650
        let bid = vec![Kline {
            open_time: 1_000,
            open: 149_600,
            high: 149_605,
            low: 149_550,
            close: 149_650,
        }];
        let ask = vec![flat(1_000, 149_700)];

        let outcome = join_and_validate(&bid, &ask);

        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.invalid, 1);
    }

    #[test]
    fn low_above_min_of_open_and_close_is_invalid() {
        // min(open, close) = 149_550, low = 149_620 > 149_550
        let bid = vec![Kline {
            open_time: 1_000,
            open: 149_600,
            high: 149_650,
            low: 149_620,
            close: 149_550,
        }];
        let ask = vec![flat(1_000, 149_700)];

        let outcome = join_and_validate(&bid, &ask);

        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.invalid, 1);
    }

    #[test]
    fn ask_open_below_bid_open_is_invalid() {
        let bid = vec![flat(1_000, 149_600)];
        let ask = vec![Kline {
            open_time: 1_000,
            open: 149_590, // < bid.open
            high: 149_610,
            low: 149_590,
            close: 149_610,
        }];

        let outcome = join_and_validate(&bid, &ask);

        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.invalid, 1);
    }

    #[test]
    fn ask_high_below_bid_high_is_invalid() {
        let bid = vec![flat(1_000, 149_600)];
        let ask = vec![Kline {
            open_time: 1_000,
            open: 149_610,
            high: 149_590, // < bid.high
            low: 149_590,
            close: 149_610,
        }];

        let outcome = join_and_validate(&bid, &ask);

        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.invalid, 1);
    }

    #[test]
    fn ask_low_below_bid_low_is_invalid() {
        let bid = vec![flat(1_000, 149_600)];
        let ask = vec![Kline {
            open_time: 1_000,
            open: 149_610,
            high: 149_610,
            low: 149_590, // < bid.low
            close: 149_610,
        }];

        let outcome = join_and_validate(&bid, &ask);

        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.invalid, 1);
    }

    #[test]
    fn ask_close_below_bid_close_is_invalid() {
        let bid = vec![flat(1_000, 149_600)];
        let ask = vec![Kline {
            open_time: 1_000,
            open: 149_610,
            high: 149_610,
            low: 149_590,
            close: 149_590, // < bid.close
        }];

        let outcome = join_and_validate(&bid, &ask);

        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.invalid, 1);
    }

    #[test]
    fn result_is_sorted_by_open_time_ascending_regardless_of_input_order() {
        let bid = vec![
            flat(3_000, 149_620),
            flat(1_000, 149_600),
            flat(2_000, 149_610),
        ];
        let ask = vec![
            flat(2_000, 149_615),
            flat(3_000, 149_625),
            flat(1_000, 149_605),
        ];

        let outcome = join_and_validate(&bid, &ask);

        assert_eq!(
            outcome.bars.iter().map(|b| b.open_time).collect::<Vec<_>>(),
            vec![1_000, 2_000, 3_000]
        );
    }

    #[test]
    fn empty_inputs_produce_empty_outcome_without_panicking() {
        let outcome = join_and_validate(&[], &[]);
        assert!(outcome.bars.is_empty());
        assert_eq!(outcome.one_sided, 0);
        assert_eq!(outcome.invalid, 0);
    }

    // ---- price string parsing (Task 2 Interfaces: 1000 倍が整数にならない値) ----

    #[test]
    fn decimal_str_to_milli_accepts_exact_three_decimal_places() {
        assert_eq!(decimal_str_to_milli("149.605"), Some(149_605));
    }

    #[test]
    fn decimal_str_to_milli_rejects_values_that_do_not_scale_to_an_integer() {
        // 149.6055 * 1000 = 149605.5, not an integer milli-yen value.
        assert_eq!(decimal_str_to_milli("149.6055"), None);
    }

    #[test]
    fn decimal_str_to_milli_rejects_non_numeric_strings() {
        assert_eq!(decimal_str_to_milli("not-a-number"), None);
    }

    // ---- openTime parsing (spec 5.2: ミリ秒の端数を黙って切り捨てない) ----

    fn raw_with_open_time(open_time: &str) -> KlineRaw {
        KlineRaw {
            open_time: open_time.to_string(),
            open: "149.605".to_string(),
            high: "149.612".to_string(),
            low: "149.601".to_string(),
            close: "149.610".to_string(),
        }
    }

    #[test]
    fn parse_kline_converts_whole_second_open_time_to_epoch_secs() {
        let k = parse_kline(&raw_with_open_time("1698451200000")).expect("whole seconds parse");
        assert_eq!(k.open_time, 1_698_451_200);
    }

    #[test]
    fn parse_kline_rejects_open_time_with_sub_second_remainder() {
        assert_eq!(parse_kline(&raw_with_open_time("1698451200500")), None);
    }

    #[test]
    fn parse_kline_rejects_negative_open_time() {
        assert_eq!(parse_kline(&raw_with_open_time("-1000")), None);
    }

    #[test]
    fn parse_klines_skips_unparseable_entries_and_keeps_the_rest() {
        let raw = vec![
            raw_with_open_time("1698451200000"),
            raw_with_open_time("1698451200500"),
        ];
        let klines = parse_klines(&raw, "20231028", PriceType::Bid);
        assert_eq!(klines.len(), 1);
        assert_eq!(klines[0].open_time, 1_698_451_200);
    }

    // ---- 取得規則の既定値 (spec 5.2) ----

    #[test]
    fn new_client_uses_one_second_interval_and_2_4_8_second_retries() {
        let client = GmoKlineClient::new("http://x");
        assert_eq!(client.min_interval, Duration::from_secs(1));
        assert_eq!(
            client.retry_delays,
            vec![
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8)
            ]
        );
    }
}
