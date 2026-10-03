//! `GmoKlineClient::fetch_day` と `backfill` の結合テスト(wiremock でモック)。
//!
//! 計画 Task 2 Step 2 の確認項目(`fetch_day`):
//! - 正常なレスポンスが `Kline` に変換される
//! - HTTP 500 が 2 回続いた後に成功すると、結果が返る(リクエストは 3 回)
//! - `status` が 5 のレスポンスが 4 回続くと `Err` になる(リクエストは 4 回)
//! - `data` が空配列のレスポンスが、空の `Vec` として成功する
//!
//! 計画 Task 2 Step 4 の確認項目(`backfill`。wiremock と `#[sqlx::test]` の両方を使う):
//! - 2 日分の BID・ASK が保存され、`BackfillReport` の件数が一致する
//! - 1 日の ASK だけが失敗し続ける場合に、`failed == ["<その日付> ASK"]` となり、
//!   他の日は保存され、戻り値が `Err(SimError::FetchIncomplete(..))` になる
//!
//! 実 API は一切叩かない。`with_timing` で間隔と再試行を 1 ミリ秒にし、テストを高速化する。

use auto_trader_sim::data::load_bars;
use auto_trader_sim::error::SimError;
use auto_trader_sim::fetch::{GmoKlineClient, PriceType, backfill};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// テスト用クライアント。間隔・再試行間隔を 1ms にして実行時間を抑える。
fn fast_client(server: &MockServer) -> GmoKlineClient {
    GmoKlineClient::new(&server.uri()).with_timing(
        Duration::from_millis(1),
        vec![
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
        ],
    )
}

fn test_date() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2023, 10, 28).unwrap()
}

#[tokio::test]
async fn fetch_day_parses_successful_response_into_klines() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "status": 0,
        "data": [{
            "openTime": "1698451200000",
            "open": "149.605",
            "high": "149.612",
            "low": "149.601",
            "close": "149.610"
        }],
        "responsetime": "2023-10-28T00:05:00.000Z"
    });
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .and(query_param("symbol", "USD_JPY"))
        .and(query_param("interval", "5min"))
        .and(query_param("priceType", "BID"))
        .and(query_param("date", "20231028"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let client = fast_client(&server);
    let klines = client
        .fetch_day(test_date(), PriceType::Bid)
        .await
        .expect("a well-formed 200 response must parse into Klines");

    assert_eq!(klines.len(), 1);
    assert_eq!(
        klines[0].open_time, 1_698_451_200,
        "openTime ms -> epoch secs"
    );
    assert_eq!(klines[0].open, 149_605, "149.605 yen -> 149605 milli-yen");
    assert_eq!(klines[0].high, 149_612);
    assert_eq!(klines[0].low, 149_601);
    assert_eq!(klines[0].close, 149_610);
}

#[tokio::test]
async fn fetch_day_enforces_min_interval_between_consecutive_requests() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"status": 0, "data": [], "responsetime": "x"});
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let min_interval = Duration::from_millis(200);
    let client = GmoKlineClient::new(&server.uri())
        .with_timing(min_interval, vec![Duration::from_millis(1); 3]);

    let started = std::time::Instant::now();
    client
        .fetch_day(test_date(), PriceType::Bid)
        .await
        .expect("first call succeeds");
    client
        .fetch_day(test_date(), PriceType::Ask)
        .await
        .expect("second call succeeds");

    assert!(
        started.elapsed() >= min_interval,
        "two consecutive requests must be at least min_interval apart, elapsed={:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn fetch_day_succeeds_with_empty_data_array() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"status": 0, "data": [], "responsetime": "x"});
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let client = fast_client(&server);
    let klines = client
        .fetch_day(test_date(), PriceType::Ask)
        .await
        .expect("empty data array is a valid (if quiet) day, not a failure");

    assert!(klines.is_empty());
}

#[tokio::test]
async fn fetch_day_retries_after_http_500_then_succeeds_on_third_request() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_in_responder = attempts.clone();
    let success_body = serde_json::json!({"status": 0, "data": [], "responsetime": "x"});

    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .respond_with(move |_req: &Request| {
            let n = attempts_in_responder.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200).set_body_json(success_body.clone())
            }
        })
        .mount(&server)
        .await;

    let client = fast_client(&server);
    let klines = client
        .fetch_day(test_date(), PriceType::Bid)
        .await
        .expect("should succeed on the 3rd attempt after two HTTP 500s");

    assert!(klines.is_empty());
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "expected exactly 3 HTTP requests (2 failures + 1 success)"
    );
}

#[tokio::test]
async fn fetch_day_fails_after_exhausting_retries_on_api_status_5() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_in_responder = attempts.clone();

    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .respond_with(move |_req: &Request| {
            attempts_in_responder.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"status": 5, "data": [], "responsetime": "x"}))
        })
        .mount(&server)
        .await;

    let client = fast_client(&server);
    let result = client.fetch_day(test_date(), PriceType::Ask).await;

    assert!(
        result.is_err(),
        "status=5 (maintenance) on every attempt must exhaust retries and fail"
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        4,
        "expected exactly 4 HTTP requests (1 initial + 3 retries)"
    );
    let message = result.unwrap_err();
    assert!(message.contains("20231028"), "date missing: {message}");
    assert!(message.contains("ASK"), "priceType missing: {message}");
    assert!(
        message.contains("api status 5"),
        "last failure reason missing: {message}"
    );
}

#[tokio::test]
async fn fetch_day_fails_after_exhausting_retries_on_malformed_json_body() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_in_responder = attempts.clone();

    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .respond_with(move |_req: &Request| {
            attempts_in_responder.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_string("not json")
        })
        .mount(&server)
        .await;

    let client = fast_client(&server);
    let message = client
        .fetch_day(test_date(), PriceType::Ask)
        .await
        .expect_err("a 200 with an unparsable body on every attempt must fail");

    assert_eq!(
        attempts.load(Ordering::SeqCst),
        4,
        "expected exactly 4 HTTP requests (1 initial + 3 retries)"
    );
    assert!(message.contains("20231028"), "date missing: {message}");
    assert!(message.contains("ASK"), "priceType missing: {message}");
}

// ---- backfill (spec 5.2 章; 計画 Task 2 Step 4) --------------------------------

/// `date`/`priceType` の組み合わせごとに固定のレスポンスを 1 件返す klines エンドポイントを
/// マウントする。`open_time_ms` は `openTime` フィールド(ミリ秒の文字列)に使う。
async fn mount_day(
    server: &MockServer,
    date: chrono::NaiveDate,
    price_type: &str,
    status: i32,
    open_time_ms: i64,
    price: &str,
) {
    let body = serde_json::json!({
        "status": status,
        "data": [{
            "openTime": open_time_ms.to_string(),
            "open": price,
            "high": price,
            "low": price,
            "close": price,
        }],
        "responsetime": "x"
    });
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .and(query_param("date", date.format("%Y%m%d").to_string()))
        .and(query_param("priceType", price_type))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn backfill_saves_two_days_of_bid_and_ask_and_reports_counts(pool: sqlx::PgPool) {
    let server = MockServer::start().await;
    let day1 = chrono::NaiveDate::from_ymd_opt(2023, 10, 28).unwrap();
    let day2 = chrono::NaiveDate::from_ymd_opt(2023, 10, 29).unwrap();

    mount_day(&server, day1, "BID", 0, 1_698_451_200_000, "149.600").await;
    mount_day(&server, day1, "ASK", 0, 1_698_451_200_000, "149.610").await;
    mount_day(&server, day2, "BID", 0, 1_698_537_600_000, "149.700").await;
    mount_day(&server, day2, "ASK", 0, 1_698_537_600_000, "149.710").await;

    let client = fast_client(&server);
    let report = backfill(&pool, &client, day1, day2)
        .await
        .expect("both days have matching BID/ASK and must succeed");

    assert_eq!(report.days, 2);
    assert_eq!(report.saved, 2, "one joined bar per day");
    assert_eq!(report.one_sided, 0);
    assert_eq!(report.invalid, 0);
    assert!(report.failed.is_empty());

    let loaded = load_bars(&pool, None)
        .await
        .expect("bars saved by backfill must be readable");
    assert_eq!(loaded.len(), 2);
}

/// 複数の足を返す klines エンドポイントをマウントする。各要素は
/// `(openTime ミリ秒, open, high, low, close)`。
async fn mount_day_with_klines(
    server: &MockServer,
    date: chrono::NaiveDate,
    price_type: &str,
    klines: &[(i64, &str, &str, &str, &str)],
) {
    let data: Vec<serde_json::Value> = klines
        .iter()
        .map(|(t, o, h, l, c)| {
            serde_json::json!({
                "openTime": t.to_string(), "open": o, "high": h, "low": l, "close": c
            })
        })
        .collect();
    let body = serde_json::json!({"status": 0, "data": data, "responsetime": "x"});
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .and(query_param("date", date.format("%Y%m%d").to_string()))
        .and(query_param("priceType", price_type))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn backfill_with_from_after_to_processes_zero_days_without_any_request(pool: sqlx::PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let from = chrono::NaiveDate::from_ymd_opt(2023, 10, 29).unwrap();
    let to = chrono::NaiveDate::from_ymd_opt(2023, 10, 28).unwrap();
    let client = fast_client(&server);
    let report = backfill(&pool, &client, from, to)
        .await
        .expect("from > to is an empty range, not an error, at this layer");

    assert_eq!(report.days, 0);
    assert_eq!(report.saved, 0);
    assert!(report.failed.is_empty());
    // wiremock は MockServer の drop 時に .expect(0) を検証する。
    drop(server);
}

#[sqlx::test(migrations = "../../migrations")]
async fn backfill_counts_one_sided_and_invalid_bars_and_saves_only_the_valid_one(
    pool: sqlx::PgPool,
) {
    let server = MockServer::start().await;
    let day = test_date();
    // t0: 正常 / t1: BID だけ / t2: 両側あるが ask_close < bid_close で不正。
    let t0 = 1_698_451_200_000;
    let t1 = t0 + 300_000;
    let t2 = t0 + 600_000;

    mount_day_with_klines(
        &server,
        day,
        "BID",
        &[
            (t0, "149.600", "149.600", "149.600", "149.600"),
            (t1, "149.600", "149.600", "149.600", "149.600"),
            (t2, "149.600", "149.600", "149.580", "149.600"),
        ],
    )
    .await;
    mount_day_with_klines(
        &server,
        day,
        "ASK",
        &[
            (t0, "149.610", "149.610", "149.610", "149.610"),
            // BID/ASK とも OHLC 整合・ask_open/high/low >= bid の各値を満たし、
            // ask_close(149.595) < bid_close(149.600) だけが違反する。
            (t2, "149.610", "149.610", "149.590", "149.595"),
        ],
    )
    .await;

    let client = fast_client(&server);
    let report = backfill(&pool, &client, day, day)
        .await
        .expect("dropped bars are not fetch failures");

    assert_eq!(report.saved, 1);
    assert_eq!(report.one_sided, 1);
    assert_eq!(report.invalid, 1);

    let loaded = load_bars(&pool, None).await.expect("load saved bars");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].open_time, t0 / 1000);
}

#[sqlx::test(migrations = "../../migrations")]
async fn backfill_continues_past_a_failed_date_and_reports_fetch_incomplete(pool: sqlx::PgPool) {
    let server = MockServer::start().await;
    let day1 = chrono::NaiveDate::from_ymd_opt(2023, 10, 28).unwrap();
    let day2 = chrono::NaiveDate::from_ymd_opt(2023, 10, 29).unwrap();

    // day1: BID と ASK がそろい、保存される。
    mount_day(&server, day1, "BID", 0, 1_698_451_200_000, "149.600").await;
    mount_day(&server, day1, "ASK", 0, 1_698_451_200_000, "149.610").await;
    // day2: BID は成功するが、ASK は status=5 を返し続け、再試行をすべて使い切って失敗する。
    mount_day(&server, day2, "BID", 0, 1_698_537_600_000, "149.700").await;
    Mock::given(method("GET"))
        .and(path("/v1/klines"))
        .and(query_param("date", day2.format("%Y%m%d").to_string()))
        .and(query_param("priceType", "ASK"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"status": 5, "data": [], "responsetime": "x"})),
        )
        .mount(&server)
        .await;

    let client = fast_client(&server);
    let err = backfill(&pool, &client, day1, day2)
        .await
        .expect_err("day2 ASK exhausts retries and must fail the whole backfill");

    match err {
        SimError::FetchIncomplete(failed) => {
            let day2_str = day2.format("%Y%m%d").to_string();
            assert_eq!(failed, vec![format!("{day2_str} ASK")]);
        }
        other => panic!("expected SimError::FetchIncomplete, got {other:?}"),
    }

    // day1 は再試行の対象ではないので、失敗した day2 とは独立に保存されている。
    let loaded = load_bars(&pool, None)
        .await
        .expect("day1 bars must still be saved despite day2 failing");
    assert_eq!(
        loaded.len(),
        1,
        "only day1's joined bar should be saved; day2 never joins because ASK failed"
    );
}
