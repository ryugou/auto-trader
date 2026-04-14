//! bitFlyer Private API の統合テスト (wiremock 使用)。
//!
//! `crates/market/src/bitflyer_private.rs` の単体テストでは sign() や
//! 型 deserialize を検証済み。ここでは HTTP 境界全体 (認証ヘッダ送信、
//! body シリアライズ、レスポンス分類) を bitFlyer ドキュメントに即した
//! ペイロードで確認する。

use auto_trader_market::bitflyer_private::{
    BitflyerApiError, BitflyerPrivateApi, ChildOrderType, SendChildOrderRequest, Side,
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
