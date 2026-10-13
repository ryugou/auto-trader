use rust_decimal_macros::dec;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

#[tokio::test]
async fn http_error_redacts_account_id_from_url() {
    let api = OandaPrivateApi::new(
        "http://127.0.0.1:1".to_string(),
        "101-001-12345-001".to_string(),
        "fake-token".to_string(),
    );
    let err = api.get_collateral().await.unwrap_err();
    assert!(
        !err.to_string().contains("101-001-12345-001"),
        "account_id leaked via connection error: {err}"
    );
}

#[tokio::test]
async fn get_executions_falls_back_to_singular_filling_transaction_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/orders/7000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": {
                "id": "7000",
                "state": "FILLED",
                "fillingTransactionID": "7001",
                "instrument": "USD_JPY",
                "units": "1000"
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/transactions/7001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transaction": {
                "id": "7001",
                "type": "ORDER_FILL",
                "units": "1000",
                "price": "148.500",
                "commission": "0.0",
                "time": "2026-04-16T05:00:01Z"
            }
        })))
        .mount(&server)
        .await;

    let api = OandaPrivateApi::new(
        server.uri(),
        "101-001-12345-001".to_string(),
        "fake-token".to_string(),
    );
    let execs = api.get_executions("USD_JPY", "7000").await.unwrap();
    assert_eq!(execs.len(), 1);
    assert_eq!(execs[0].price, dec!(148.500));
}

#[tokio::test]
async fn get_positions_returns_both_sides_when_hedged() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/positions/USD_JPY"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "position": {
                "instrument": "USD_JPY",
                "long":  { "units": "1000", "averagePrice": "148.500" },
                "short": { "units": "-500", "averagePrice": "149.000" }
            }
        })))
        .mount(&server)
        .await;

    let api = OandaPrivateApi::new(
        server.uri(),
        "101-001-12345-001".to_string(),
        "fake-token".to_string(),
    );
    let ps = api.get_positions("USD_JPY").await.unwrap();
    assert_eq!(ps.len(), 2, "both long and short legs must be returned");
    assert!(ps.iter().any(|p| p.side == "BUY" && p.size == dec!(1000)));
    assert!(ps.iter().any(|p| p.side == "SELL" && p.size == dec!(500)));
}

#[tokio::test]
async fn positions_path_escapes_special_chars() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/positions/USD%2FJPY"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "position": {
                "long":  { "units": "0", "averagePrice": "0" },
                "short": { "units": "0", "averagePrice": "0" }
            }
        })))
        .mount(&server)
        .await;

    let api = OandaPrivateApi::new(
        server.uri(),
        "101-001-12345-001".to_string(),
        "fake-token".to_string(),
    );
    let ps = api.get_positions("USD/JPY").await.unwrap();
    assert_eq!(ps.len(), 0);
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

    let api = OandaPrivateApi::new(
        server.uri(),
        "101-001-12345-001".to_string(),
        "fake-token".to_string(),
    );
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

    let api = OandaPrivateApi::new(
        server.uri(),
        "101-001-12345-001".to_string(),
        "fake-token".to_string(),
    );
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

    let api = OandaPrivateApi::new(
        server.uri(),
        "101-001-12345-001".to_string(),
        "fake-token".to_string(),
    );
    let err = api.get_positions("USD/JPY").await.unwrap_err();
    assert!(
        !err.to_string().contains("101-001-12345-001"),
        "leaked: {err}"
    );
}
