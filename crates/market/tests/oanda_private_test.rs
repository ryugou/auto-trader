//! wiremock-based tests for OandaPrivateApi. Does NOT hit live OANDA.

use auto_trader_market::bitflyer_private::{ChildOrderType, SendChildOrderRequest, Side};
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::oanda_private::OandaPrivateApi;
use rust_decimal_macros::dec;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn api(base_url: &str) -> OandaPrivateApi {
    OandaPrivateApi::new(
        base_url.to_string(),
        "101-001-12345-001".to_string(),
        "test-token".to_string(),
    )
}

#[tokio::test]
async fn send_child_order_returns_order_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/accounts/101-001-12345-001/orders"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "orderCreateTransaction": { "id": "6372" }
        })))
        .mount(&server)
        .await;

    let api = api(&server.uri());
    let resp = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".to_string(),
            child_order_type: ChildOrderType::Market,
            side: Side::Buy,
            size: dec!(1000),
            price: None,
            minute_to_expire: None,
            time_in_force: None,
        })
        .await
        .unwrap();
    assert_eq!(resp.child_order_acceptance_id, "6372");
}

#[tokio::test]
async fn get_collateral_parses_summary() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "account": {
                "balance": "30500.123",
                "unrealizedPL": "0.0",
                "marginUsed": "120.5",
                "marginAvailable": "30379.6"
            }
        })))
        .mount(&server)
        .await;

    let api = api(&server.uri());
    let col = api.get_collateral().await.unwrap();
    assert_eq!(col.collateral, dec!(30500.123));
    assert_eq!(col.open_position_pnl, dec!(0.0));
    assert_eq!(col.require_collateral, dec!(120.5));
}

#[tokio::test]
async fn get_positions_splits_long_and_short() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/positions/USD_JPY"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "position": {
                "instrument": "USD_JPY",
                "long":  { "units": "1000",  "averagePrice": "148.500" },
                "short": { "units": "0",     "averagePrice": "0" }
            }
        })))
        .mount(&server)
        .await;

    let api = api(&server.uri());
    let ps = api.get_positions("USD_JPY").await.unwrap();
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].side, "BUY");
    assert_eq!(ps[0].size, dec!(1000));
    assert_eq!(ps[0].price, dec!(148.500));
}

#[tokio::test]
async fn send_child_order_rejects_zero_size() {
    let server = MockServer::start().await;
    let api = api(&server.uri());
    let err = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".to_string(),
            child_order_type: ChildOrderType::Market,
            side: Side::Buy,
            size: dec!(0),
            price: None,
            minute_to_expire: None,
            time_in_force: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("size"));
}

#[tokio::test]
async fn send_child_order_rejects_limit() {
    let server = MockServer::start().await;
    let api = api(&server.uri());
    let err = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".to_string(),
            child_order_type: ChildOrderType::Limit,
            side: Side::Buy,
            size: dec!(1000),
            price: Some(dec!(148.5)),
            minute_to_expire: None,
            time_in_force: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("MARKET") || err.to_string().contains("LIMIT"));
}

#[tokio::test]
async fn get_executions_follows_fill_transaction() {
    let server = MockServer::start().await;
    // 1st hop: GET /orders/{id} returns FILLED order with fill IDs
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/orders/6372"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": {
                "id": "6372",
                "state": "FILLED",
                "fillingTransactionIDs": ["6373"],
                "instrument": "USD_JPY",
                "units": "1000",
                "createTime": "2026-04-16T05:00:00Z",
                "filledTime": "2026-04-16T05:00:01Z",
            }
        })))
        .mount(&server)
        .await;
    // 2nd hop: GET /transactions/6373 returns ORDER_FILL with price
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/transactions/6373"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transaction": {
                "id": "6373",
                "type": "ORDER_FILL",
                "units": "1000",
                "price": "148.523",
                "commission": "0.0",
                "time": "2026-04-16T05:00:01Z",
            }
        })))
        .mount(&server)
        .await;

    let api = api(&server.uri());
    let execs = api.get_executions("USD_JPY", "6372").await.unwrap();
    assert_eq!(execs.len(), 1);
    assert_eq!(execs[0].price, dec!(148.523));
    assert_eq!(execs[0].size, dec!(1000));
    assert_eq!(execs[0].side, "BUY");
}

#[tokio::test]
async fn get_executions_empty_when_not_filled() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/orders/6372"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": {
                "id": "6372",
                "state": "PENDING",
                "instrument": "USD_JPY",
                "units": "1000",
            }
        })))
        .mount(&server)
        .await;

    let api = api(&server.uri());
    let execs = api.get_executions("USD_JPY", "6372").await.unwrap();
    assert!(execs.is_empty());
}

#[tokio::test]
async fn cancel_child_order_sends_put() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v3/accounts/101-001-12345-001/orders/6372/cancel"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "orderCancelTransaction": { "id": "6373" }
        })))
        .mount(&server)
        .await;

    let api = api(&server.uri());
    api.cancel_child_order("USD_JPY", "6372").await.unwrap();
}

#[tokio::test]
async fn send_child_order_rejects_fractional_size() {
    let server = MockServer::start().await;
    let api = api(&server.uri());
    let err = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".to_string(),
            child_order_type: ChildOrderType::Market,
            side: Side::Buy,
            size: dec!(1000.5),
            price: None,
            minute_to_expire: None,
            time_in_force: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("integer"), "err={err}");
}

#[tokio::test]
async fn send_child_order_rejects_negative_size() {
    let server = MockServer::start().await;
    let api = api(&server.uri());
    let err = api
        .send_child_order(SendChildOrderRequest {
            product_code: "USD_JPY".to_string(),
            child_order_type: ChildOrderType::Market,
            side: Side::Buy,
            size: dec!(-100),
            price: None,
            minute_to_expire: None,
            time_in_force: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("size"), "err={err}");
}

#[tokio::test]
async fn get_executions_handles_sell_side() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/orders/6400"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": {
                "id": "6400",
                "state": "FILLED",
                "fillingTransactionIDs": ["6401"],
                "instrument": "USD_JPY",
                "units": "-1000",
                "createTime": "2026-04-16T05:00:00Z",
                "filledTime": "2026-04-16T05:00:01Z",
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/transactions/6401"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transaction": {
                "id": "6401",
                "type": "ORDER_FILL",
                "units": "-1000",
                "price": "148.500",
                "commission": "0.0",
                "time": "2026-04-16T05:00:01Z",
            }
        })))
        .mount(&server)
        .await;

    let api = api(&server.uri());
    let execs = api.get_executions("USD_JPY", "6400").await.unwrap();
    assert_eq!(execs.len(), 1);
    assert_eq!(execs[0].side, "SELL");
    assert_eq!(execs[0].size, dec!(1000));
}

#[tokio::test]
async fn get_executions_handles_multiple_fills() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/orders/6500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": {
                "id": "6500",
                "state": "FILLED",
                "fillingTransactionIDs": ["6501", "6502"],
                "instrument": "USD_JPY",
                "units": "2000",
                "createTime": "2026-04-16T05:00:00Z",
                "filledTime": "2026-04-16T05:00:01Z",
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/transactions/6501"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transaction": {
                "id": "6501",
                "type": "ORDER_FILL",
                "units": "1500",
                "price": "148.500",
                "commission": "0.0",
                "time": "2026-04-16T05:00:01Z",
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/transactions/6502"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transaction": {
                "id": "6502",
                "type": "ORDER_FILL",
                "units": "500",
                "price": "148.510",
                "commission": "0.0",
                "time": "2026-04-16T05:00:01Z",
            }
        })))
        .mount(&server)
        .await;

    let api = api(&server.uri());
    let execs = api.get_executions("USD_JPY", "6500").await.unwrap();
    assert_eq!(execs.len(), 2);
    assert_eq!(execs[0].size, dec!(1500));
    assert_eq!(execs[0].price, dec!(148.500));
    assert_eq!(execs[1].size, dec!(500));
    assert_eq!(execs[1].price, dec!(148.510));
}

#[tokio::test]
async fn get_executions_errors_on_cancelled_order() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/orders/6600"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": {
                "id": "6600",
                "state": "CANCELLED",
                "instrument": "USD_JPY",
                "units": "1000",
                "cancelledTime": "2026-04-16T05:00:01Z",
                "cancellingTransactionID": "6601",
            }
        })))
        .mount(&server)
        .await;

    let api = api(&server.uri());
    let err = api.get_executions("USD_JPY", "6600").await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("CANCELLED"), "expected CANCELLED in: {msg}");
    assert!(msg.contains("6600"), "expected order id 6600 in: {msg}");
}

#[tokio::test]
async fn get_executions_errors_when_filled_without_transaction_ids() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/accounts/101-001-12345-001/orders/6700"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order": {
                "id": "6700",
                "state": "FILLED",
                "instrument": "USD_JPY",
                "units": "1000"
            }
        })))
        .mount(&server)
        .await;
    let api = api(&server.uri());
    let err = api.get_executions("USD_JPY", "6700").await.unwrap_err();
    assert!(err.to_string().contains("missing fillingTransaction"));
}
