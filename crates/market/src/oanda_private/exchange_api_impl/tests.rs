use rust_decimal_macros::dec;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::oanda_private::test_support::api_for;

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

    let api = api_for(&server);
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

    let api = api_for(&server);
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

    let api = api_for(&server);
    let ps = api.get_positions("USD/JPY").await.unwrap();
    assert_eq!(ps.len(), 0);
}
