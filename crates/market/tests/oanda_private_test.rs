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
