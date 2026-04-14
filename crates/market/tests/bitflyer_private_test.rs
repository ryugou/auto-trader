//! bitFlyer Private API の統合テスト (wiremock 使用)。
//!
//! `crates/market/src/bitflyer_private.rs` の単体テストでは sign() や
//! 型 deserialize を検証済み。ここでは HTTP 境界全体 (認証ヘッダ送信、
//! body シリアライズ、レスポンス分類) を bitFlyer ドキュメントに即した
//! ペイロードで確認する。

use auto_trader_market::bitflyer_private::{
    BitflyerApiError, BitflyerPrivateApi, ChildOrderType, RateLimiter, SendChildOrderRequest, Side,
};
use rust_decimal_macros::dec;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client_for(server: &MockServer) -> BitflyerPrivateApi {
    BitflyerPrivateApi::new_for_test(
        server.uri(),
        "test-key".to_string(),
        "test-secret".to_string(),
    )
}

#[tokio::test]
async fn send_child_order_market_order_returns_acceptance_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/sendchildorder"))
        .and(header_exists("ACCESS-KEY"))
        .and(header_exists("ACCESS-TIMESTAMP"))
        .and(header_exists("ACCESS-SIGN"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"child_order_acceptance_id":"JRF20260414-050237-639234"}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let req = SendChildOrderRequest {
        product_code: "FX_BTC_JPY".to_string(),
        child_order_type: ChildOrderType::Market,
        side: Side::Buy,
        size: dec!(0.01),
        price: None,
        minute_to_expire: None,
        time_in_force: None,
    };
    let resp = api.send_child_order(req).await.unwrap();
    assert_eq!(resp.child_order_acceptance_id, "JRF20260414-050237-639234");
}

#[tokio::test]
async fn send_child_order_insufficient_funds_maps_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/sendchildorder"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                r#"{"status":-205,"error_message":"Insufficient fund","data":null}"#,
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let req = SendChildOrderRequest {
        product_code: "FX_BTC_JPY".to_string(),
        child_order_type: ChildOrderType::Market,
        side: Side::Buy,
        size: dec!(10),
        price: None,
        minute_to_expire: None,
        time_in_force: None,
    };
    let err = api.send_child_order(req).await.unwrap_err();
    match err {
        BitflyerApiError::InsufficientFunds(msg) => {
            assert!(msg.contains("Insufficient"), "unexpected msg: {msg}");
        }
        other => panic!("expected InsufficientFunds, got {other:?}"),
    }
}

#[tokio::test]
async fn send_child_order_invalid_api_key_maps_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/sendchildorder"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(
                r#"{"status":-201,"error_message":"Invalid API key","data":null}"#,
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let req = SendChildOrderRequest {
        product_code: "FX_BTC_JPY".to_string(),
        child_order_type: ChildOrderType::Market,
        side: Side::Buy,
        size: dec!(0.01),
        price: None,
        minute_to_expire: None,
        time_in_force: None,
    };
    let err = api.send_child_order(req).await.unwrap_err();
    assert!(matches!(err, BitflyerApiError::InvalidApiKey));
}

use wiremock::matchers::query_param;

#[tokio::test]
async fn get_child_orders_returns_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getchildorders"))
        .and(query_param("product_code", "FX_BTC_JPY"))
        .and(query_param(
            "child_order_acceptance_id",
            "JRF20260414-050237-639234",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{
                "id": 1,
                "child_order_id": "JOR20260414-050237-639234",
                "product_code": "FX_BTC_JPY",
                "side": "BUY",
                "child_order_type": "MARKET",
                "price": "0",
                "average_price": "11500000",
                "size": "0.01",
                "child_order_state": "COMPLETED",
                "expire_date": "2026-05-14T07:25:47",
                "child_order_date": "2026-04-14T08:45:47",
                "child_order_acceptance_id": "JRF20260414-050237-639234",
                "outstanding_size": "0",
                "cancel_size": "0",
                "executed_size": "0.01",
                "total_commission": "0"
            }]"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let orders = api
        .get_child_orders("FX_BTC_JPY", "JRF20260414-050237-639234")
        .await
        .unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].child_order_id, "JOR20260414-050237-639234");
    assert_eq!(orders[0].executed_size, dec!(0.01));
}

#[tokio::test]
async fn get_child_orders_empty_list_is_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getchildorders"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let orders = api
        .get_child_orders("FX_BTC_JPY", "unknown_id")
        .await
        .unwrap();
    assert!(orders.is_empty());
}

#[tokio::test]
async fn get_executions_returns_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getexecutions"))
        .and(query_param("product_code", "FX_BTC_JPY"))
        .and(query_param(
            "child_order_acceptance_id",
            "JRF20260414-050237-639234",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{
                "id": 99,
                "child_order_id": "JOR20260414-050237-639234",
                "side": "BUY",
                "price": "11500000",
                "size": "0.01",
                "commission": "0",
                "exec_date": "2026-04-14T09:57:40.397",
                "child_order_acceptance_id": "JRF20260414-050237-639234"
            }]"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let execs = api
        .get_executions("FX_BTC_JPY", "JRF20260414-050237-639234")
        .await
        .unwrap();
    assert_eq!(execs.len(), 1);
    assert_eq!(execs[0].price, dec!(11500000));
}

#[tokio::test]
async fn get_positions_returns_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getpositions"))
        .and(query_param("product_code", "FX_BTC_JPY"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"[{
                "product_code": "FX_BTC_JPY",
                "side": "BUY",
                "price": "11500000",
                "size": "0.01",
                "commission": "0",
                "swap_point_accumulate": "0",
                "require_collateral": "57500",
                "open_date": "2026-04-14T10:04:45.011",
                "leverage": "2",
                "pnl": "0",
                "sfd": "0"
            }]"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let positions = api.get_positions("FX_BTC_JPY").await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].size, dec!(0.01));
    assert_eq!(positions[0].product_code, "FX_BTC_JPY");
}

#[tokio::test]
async fn get_collateral_returns_struct() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getcollateral"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{
                "collateral": "30000",
                "open_position_pnl": "-123",
                "require_collateral": "15000",
                "keep_rate": "2.0"
            }"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let c = api.get_collateral().await.unwrap();
    assert_eq!(c.collateral, dec!(30000));
    assert_eq!(c.open_position_pnl, dec!(-123));
}

#[tokio::test]
async fn cancel_child_order_returns_unit_on_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/cancelchildorder"))
        .and(header_exists("ACCESS-SIGN"))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    api.cancel_child_order("FX_BTC_JPY", "JRF20260414-050237-639234")
        .await
        .unwrap();
}

#[tokio::test]
async fn cancel_child_order_unknown_id_maps_to_order_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/me/cancelchildorder"))
        .respond_with(
            ResponseTemplate::new(404).set_body_string(
                r#"{"status":-208,"error_message":"Order not found","data":null}"#,
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api = client_for(&server);
    let err = api
        .cancel_child_order("FX_BTC_JPY", "bogus")
        .await
        .unwrap_err();
    assert!(matches!(err, BitflyerApiError::OrderNotFound(_)));
}

/// Rate limiter: バケットが空になったとき 3 件目のリクエストが待たされること。
///
/// 1 秒 2 件のバケットを with_rate_limiter() で注入し、3 件連続呼び出しで
/// 3 件目が少なくとも 400ms 待たされることを計測する。
#[tokio::test]
async fn rate_limit_waits_when_bucket_empty() {
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let server = MockServer::start().await;
    // 3 回呼ばれる GET /v1/me/getcollateral を stub
    Mock::given(method("GET"))
        .and(path("/v1/me/getcollateral"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"collateral":"100000","open_position_pnl":"0","require_collateral":"0","keep_rate":"0"}"#,
        ))
        .expect(3)
        .mount(&server)
        .await;

    // 1 秒 2 件バケット (burst なし = burst 1) を注入
    let limiter: Arc<RateLimiter> = Arc::new(governor::RateLimiter::direct(
        governor::Quota::per_second(NonZeroU32::new(2).unwrap()),
    ));
    let api = BitflyerPrivateApi::new_for_test(
        server.uri(),
        "test-key".to_string(),
        "test-secret".to_string(),
    )
    .with_rate_limiter(limiter);

    let start = Instant::now();
    api.get_collateral().await.unwrap(); // 1 件目
    api.get_collateral().await.unwrap(); // 2 件目 (バケット満杯で即通過)
    api.get_collateral().await.unwrap(); // 3 件目 (バケット空 → 待つ)
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(400),
        "3 requests with 2 req/s limiter should take at least 400ms, got {:?}",
        elapsed
    );
}
