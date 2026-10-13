use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::bitflyer_private::{ChildOrderType, SendChildOrderRequest};
use crate::exchange_api::ExchangeApi as _;
use crate::oanda_private::test_support::{api_at, api_for};

#[tokio::test]
async fn http_error_redacts_account_id_from_url() {
    let api = api_at("http://127.0.0.1:1");
    let err = api.get_collateral().await.unwrap_err();
    assert!(
        !err.to_string().contains("101-001-12345-001"),
        "account_id leaked via connection error: {err}"
    );
}

#[tokio::test]
async fn send_json_redacts_account_id_in_error_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/accounts/101-001-12345-001/orders"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "errorMessage": "Invalid request for accountID 101-001-12345-001",
            "accountID": "101-001-12345-001"
        })))
        .mount(&server)
        .await;

    let api = api_for(&server);
    let err = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".to_string(),
            child_order_type: ChildOrderType::Market,
            side: crate::bitflyer_private::Side::Buy,
            size: rust_decimal::Decimal::from(1000),
            price: None,
            minute_to_expire: None,
            time_in_force: None,
            close_position_id: None,
        })
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains("101-001-12345-001"),
        "account_id leaked: {msg}"
    );
    assert!(msg.contains("REDACTED"), "redaction marker missing: {msg}");
}

#[tokio::test]
async fn send_json_redacts_account_id_even_past_truncation_boundary() {
    let server = MockServer::start().await;
    let padding = "x".repeat(550);
    let body = format!(
        "{{\"errorMessage\": \"{}\", \"accountID\": \"101-001-12345-001\"}}",
        padding
    );
    Mock::given(method("POST"))
        .and(path("/v3/accounts/101-001-12345-001/orders"))
        .respond_with(ResponseTemplate::new(400).set_body_string(body))
        .mount(&server)
        .await;

    let api = api_for(&server);
    let err = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".to_string(),
            child_order_type: ChildOrderType::Market,
            side: crate::bitflyer_private::Side::Buy,
            size: rust_decimal::Decimal::from(1000),
            price: None,
            minute_to_expire: None,
            time_in_force: None,
            close_position_id: None,
        })
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains("101-001-12345-001"),
        "account_id leaked past truncation: {msg}"
    );
}

#[tokio::test]
async fn parse_error_redacts_account_id_in_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/positions/USD%2FJPY"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "wrong_field": "whatever 101-001-12345-001"
        })))
        .mount(&server)
        .await;

    let api = api_for(&server);
    let err = api.get_positions("USD/JPY").await.unwrap_err();
    assert!(
        !err.to_string().contains("101-001-12345-001"),
        "leaked: {err}"
    );
}
