use rust_decimal_macros::dec;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::bitflyer_private::{ChildOrderType, Side};
use crate::exchange_api::ExchangeApi as _;
use crate::gmo_fx_private::test_support::{api_for, market_order, offline_api};

#[tokio::test]
async fn send_open_order_posts_v1_order_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/private/v1/order"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0, "data": { "rootOrderId": 9876 }
        })))
        .mount(&server)
        .await;
    let api = api_for(&server);
    let resp = api
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
        .unwrap();
    assert_eq!(resp.child_order_acceptance_id, "9876");
}

#[tokio::test]
async fn send_close_order_posts_v1_close_order_with_position_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/private/v1/closeOrder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0, "data": { "rootOrderId": 5555 }
        })))
        .mount(&server)
        .await;
    let api = api_for(&server);
    let resp = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".into(),
            child_order_type: ChildOrderType::Market,
            side: Side::Sell,
            size: dec!(1000),
            price: None,
            minute_to_expire: None,
            time_in_force: None,
            close_position_id: Some("123".into()),
        })
        .await
        .unwrap();
    assert_eq!(resp.child_order_acceptance_id, "5555");
}

#[tokio::test]
async fn limit_open_order_is_rejected() {
    let api = offline_api();
    let err = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".into(),
            child_order_type: ChildOrderType::Limit,
            side: Side::Buy,
            size: dec!(1000),
            price: Some(dec!(150.0)),
            minute_to_expire: None,
            time_in_force: None,
            close_position_id: None,
        })
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("unsupported order type: LIMIT"),
        "should mention unsupported order type: {err}"
    );
}

#[tokio::test]
async fn send_child_order_rejects_invalid_market_order_inputs() {
    let cases = [
        (
            market_order(None, Some("abc")),
            "invalid close_position_id: not a u64",
        ),
        (
            market_order(Some(dec!(150.0)), None),
            "invalid price: must be none for a market order",
        ),
    ];
    let api = offline_api();
    for (req, expected) in cases {
        let err = api.send_child_order(req).await.unwrap_err();
        assert!(err.to_string().contains(expected), "err={err}");
    }
}
