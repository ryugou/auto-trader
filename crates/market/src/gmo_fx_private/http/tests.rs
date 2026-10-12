use rust_decimal_macros::dec;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::bitflyer_private::{ChildOrderType, SendChildOrderRequest, Side};
use crate::exchange_api::ExchangeApi as _;
use crate::gmo_fx_private::test_support::{api_for, market_order};

#[test]
fn sign_matches_known_hmac_sha256_vector() {
    let sig = sign("test_secret", 1700000000, "GET", "/v1/account/assets", "");
    assert_eq!(
        sig,
        "1c3cbd89febce462e71e5f7d265ab674f617e6d4449ba5e665b46d1234bedbca"
    );
}

#[test]
fn sign_includes_post_body() {
    let with_body = sign("s", 1, "POST", "/p", "{}");
    let without_body = sign("s", 1, "POST", "/p", "");
    assert_ne!(with_body, without_body);
}

#[test]
fn sign_path_for_strips_query_string() {
    assert_eq!(
        sign_path_for("/v1/openPositions?symbol=USD_JPY"),
        "/v1/openPositions"
    );
    assert_eq!(sign_path_for("/v1/account/assets"), "/v1/account/assets");
    assert_eq!(sign_path_for("/v1/x?a=1?b=2"), "/v1/x");
}

#[test]
fn sign_produces_same_hash_regardless_of_query_string() {
    let with_query = sign(
        "s",
        1700000000,
        "GET",
        sign_path_for("/v1/openPositions?symbol=USD_JPY"),
        "",
    );
    let without_query = sign("s", 1700000000, "GET", "/v1/openPositions", "");
    assert_eq!(with_query, without_query);
}

#[tokio::test]
async fn signed_request_attaches_api_key_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/account/assets"))
        .and(header("API-KEY", "k"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": {
                "equity": "100000", "availableAmount": "90000",
                "balance": "100000", "estimatedTradeFee": "0",
                "margin": "10000", "marginCallStatus": "NORMAL",
                "marginRatio": "10.0", "positionLossGain": "0",
                "totalSwap": "0", "transferableAmount": "90000"
            }
        })))
        .mount(&server)
        .await;
    let api = api_for(&server);
    let collateral = api.get_collateral().await.unwrap();
    assert_eq!(collateral.collateral, dec!(100000));
    assert_eq!(collateral.keep_rate, dec!(10.0));
}

#[tokio::test]
async fn api_error_status_propagates_as_anyhow_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/private/v1/order"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 1,
            "messages": [{ "messageCode": "ERR-100", "messageString": "invalid" }],
            "data": null
        })))
        .mount(&server)
        .await;
    let api = api_for(&server);
    let err = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".into(),
            child_order_type: ChildOrderType::Market,
            side: Side::Buy,
            size: dec!(1000),
            price: None,
            minute_to_expire: None,
            time_in_force: None,
            close_position_id: None,
        })
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("status=1"), "msg: {msg}");
}

#[tokio::test]
async fn null_data_with_zero_status_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/private/v1/order"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0, "data": null
        })))
        .mount(&server)
        .await;
    let api = api_for(&server);
    let err = api
        .send_child_order(market_order(None, None))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("invalid response: data is null"),
        "err={err}"
    );
}

#[tokio::test]
async fn unparseable_response_body_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/private/v1/order"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let api = api_for(&server);
    let err = api
        .send_child_order(market_order(None, None))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("failed to parse response body: "), "err={err}");
    assert!(msg.contains("not json"), "err={err}");
}
