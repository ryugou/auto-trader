use rust_decimal_macros::dec;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::bitflyer_private::ChildOrderType;

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

#[test]
fn open_order_request_serializes_to_camelcase() {
    let req = GmoOrderRequest {
        symbol: "USD_JPY".into(),
        side: GmoSide::Buy,
        execution_type: GmoExecutionType::Market,
        size: "1000".into(),
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["symbol"], "USD_JPY");
    assert_eq!(json["side"], "BUY");
    assert_eq!(json["executionType"], "MARKET");
    assert_eq!(json["size"], "1000");
}

#[test]
fn close_order_request_serializes_settle_position() {
    let req = GmoCloseRequest {
        symbol: "USD_JPY".into(),
        side: GmoSide::Sell,
        execution_type: GmoExecutionType::Market,
        settle_position: vec![GmoSettlePosition {
            position_id: 12345,
            size: "1000".into(),
        }],
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["settlePosition"][0]["positionId"], 12345);
    assert_eq!(json["settlePosition"][0]["size"], "1000");
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
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let collateral = api.get_collateral().await.unwrap();
    assert_eq!(collateral.collateral, dec!(100000));
    assert_eq!(collateral.keep_rate, dec!(10.0));
}

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
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
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
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
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
async fn resolve_position_id_returns_newest_position_after_cutoff() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/openPositions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": {
                "list": [
                    { "positionId": 100, "symbol": "USD_JPY", "side": "BUY",
                      "size": "1000", "price": "150.0",
                      "timestamp": "2026-05-15T10:00:00Z" },
                    { "positionId": 101, "symbol": "USD_JPY", "side": "BUY",
                      "size": "1000", "price": "150.1",
                      "timestamp": "2026-05-15T11:00:00Z" }
                ]
            }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let after = "2026-05-15T09:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let pid = api
        .resolve_position_id("USD_JPY", after, Side::Buy, dec!(1000))
        .await
        .unwrap();
    assert_eq!(pid.as_deref(), Some("101"));
}

#[tokio::test]
async fn resolve_position_id_excludes_positions_before_cutoff() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/openPositions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": {
                "list": [
                    { "positionId": 100, "symbol": "USD_JPY", "side": "BUY",
                      "size": "1000", "price": "150.0",
                      "timestamp": "2026-05-15T08:00:00Z" }
                ]
            }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let after = "2026-05-15T09:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let pid = api
        .resolve_position_id("USD_JPY", after, Side::Buy, dec!(1000))
        .await
        .unwrap();
    assert_eq!(pid, None);
}

#[tokio::test]
async fn resolve_position_id_skips_unrelated_side_and_size() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/openPositions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": {
                "list": [
                    { "positionId": 200, "symbol": "USD_JPY", "side": "SELL",
                      "size": "1000", "price": "150.0",
                      "timestamp": "2026-05-15T12:00:00Z" },
                    { "positionId": 201, "symbol": "USD_JPY", "side": "BUY",
                      "size": "500", "price": "150.0",
                      "timestamp": "2026-05-15T11:30:00Z" },
                    { "positionId": 202, "symbol": "USD_JPY", "side": "BUY",
                      "size": "1000", "price": "150.0",
                      "timestamp": "2026-05-15T11:00:00Z" }
                ]
            }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let after = "2026-05-15T09:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let pid = api
        .resolve_position_id("USD_JPY", after, Side::Buy, dec!(1000))
        .await
        .unwrap();
    assert_eq!(pid.as_deref(), Some("202"));
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
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
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
async fn requires_close_position_id_is_true() {
    let api = GmoFxPrivateApi::new("k".into(), "s".into());
    assert!(api.requires_close_position_id());
}

#[tokio::test]
async fn limit_open_order_is_rejected() {
    let api = GmoFxPrivateApi::new("k".into(), "s".into());
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
async fn get_child_orders_maps_gmo_status_to_state() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": {
                "list": [
                    {
                        "orderId": 9876,
                        "symbol": "USD_JPY",
                        "side": "BUY",
                        "executionType": "MARKET",
                        "size": "1000",
                        "executedSize": "1000",
                        "price": "150.0",
                        "status": "EXECUTED",
                        "timestamp": "2026-05-15T10:00:00Z"
                    }
                ]
            }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let orders = api.get_child_orders("USD_JPY", "9876").await.unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(
        orders[0].child_order_state,
        crate::bitflyer_private::ChildOrderState::Completed
    );
    assert_eq!(orders[0].id, 9876);
    assert_eq!(orders[0].executed_size, dec!(1000));
}

#[test]
fn gmo_status_to_state_maps_each_terminal_status() {
    assert_eq!(gmo_status_to_state("EXECUTED"), ChildOrderState::Completed);
    assert_eq!(gmo_status_to_state("CANCELED"), ChildOrderState::Canceled);
    assert_eq!(gmo_status_to_state("EXPIRED"), ChildOrderState::Expired);
    assert_eq!(gmo_status_to_state("REJECTED"), ChildOrderState::Rejected);
    assert_eq!(gmo_status_to_state("ORDERED"), ChildOrderState::Active);
    assert_eq!(gmo_status_to_state("WAITING"), ChildOrderState::Active);
}

#[test]
fn gmo_order_defaults_executed_size_when_field_is_omitted() {
    let json = r#"{
        "orderId": 9876, "symbol": "USD_JPY", "side": "BUY",
        "executionType": "MARKET", "size": "1000", "price": "150.0",
        "status": "ORDERED", "timestamp": "2026-05-15T10:00:00Z"
    }"#;
    let order: GmoOrder = serde_json::from_str(json).unwrap();
    assert_eq!(order.executed_size, Decimal::ZERO);
}

#[tokio::test]
async fn cancel_child_order_posts_order_id_only_without_symbol() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/private/v1/cancelOrder"))
        .and(body_json(serde_json::json!({ "orderId": 54321 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0, "data": 54321
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    api.cancel_child_order("USD_JPY", "54321").await.unwrap();
}

#[test]
fn stop_close_request_serializes_stop_price_and_settle_position() {
    let req = GmoStopCloseRequest {
        symbol: "USD_JPY".into(),
        side: GmoSide::Sell,
        execution_type: GmoExecutionType::Stop,
        price: "156.500".into(),
        settle_position: vec![GmoSettlePosition {
            position_id: 123456,
            size: "1592".into(),
        }],
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["symbol"], "USD_JPY");
    assert_eq!(json["side"], "SELL");
    assert_eq!(json["executionType"], "STOP");
    assert_eq!(json["price"], "156.500");
    assert_eq!(json["settlePosition"][0]["positionId"], 123456);
    assert_eq!(json["settlePosition"][0]["size"], "1592");
}

#[tokio::test]
async fn place_stop_order_posts_close_order_stop_and_returns_order_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/private/v1/closeOrder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0, "data": { "rootOrderId": 778899 }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let id = api
        .place_stop_order(
            "USD_JPY",
            Side::Sell,
            dec!(1592),
            dec!(156.500),
            Some("123456"),
        )
        .await
        .unwrap();
    assert_eq!(id, "778899");
}

#[tokio::test]
async fn place_stop_order_without_position_id_bails() {
    let api = GmoFxPrivateApi::new("k".into(), "s".into());
    let err = api
        .place_stop_order("USD_JPY", Side::Sell, dec!(1592), dec!(156.5), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("invalid position_id: required for a netting close"),
        "err={err}"
    );
}

#[tokio::test]
async fn cancel_stop_order_posts_cancel_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/private/v1/cancelOrder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0, "data": 778899
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    api.cancel_stop_order("USD_JPY", "778899").await.unwrap();
}

#[tokio::test]
async fn stop_order_status_active_for_ordered() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": { "list": [{
                "orderId": 778899, "symbol": "USD_JPY", "side": "SELL",
                "executionType": "STOP", "size": "1592", "executedSize": "0",
                "price": "156.500", "status": "ORDERED",
                "timestamp": "2026-07-07T00:00:00Z"
            }] }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let status = api.stop_order_status("USD_JPY", "778899").await.unwrap();
    assert_eq!(status, StopOrderStatus::Active);
}

#[tokio::test]
async fn stop_order_status_gone_for_empty_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0, "data": { "list": [] }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let status = api.stop_order_status("USD_JPY", "778899").await.unwrap();
    assert_eq!(status, StopOrderStatus::Gone);
}

#[tokio::test]
async fn stop_order_status_gone_for_canceled_order() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": { "list": [{
                "orderId": 778899, "symbol": "USD_JPY", "side": "SELL",
                "executionType": "STOP", "size": "1592", "executedSize": "0",
                "price": "156.500", "status": "CANCELED",
                "timestamp": "2026-07-07T00:00:00Z"
            }] }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let status = api.stop_order_status("USD_JPY", "778899").await.unwrap();
    assert_eq!(status, StopOrderStatus::Gone);
}

#[tokio::test]
async fn stop_order_status_executed_aggregates_executions() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": { "list": [{
                "orderId": 778899, "symbol": "USD_JPY", "side": "SELL",
                "executionType": "STOP", "size": "1592", "executedSize": "1592",
                "price": "156.500", "status": "EXECUTED",
                "timestamp": "2026-07-07T00:00:00Z"
            }] }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/private/v1/executions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": { "list": [
                { "executionId": 1, "orderId": 778899, "positionId": 123456,
                  "symbol": "USD_JPY", "side": "SELL", "settleType": "CLOSE",
                  "size": "796", "price": "156.510", "lossGain": "0", "fee": "3",
                  "timestamp": "2026-07-07T00:00:00Z" },
                { "executionId": 2, "orderId": 778899, "positionId": 123456,
                  "symbol": "USD_JPY", "side": "SELL", "settleType": "CLOSE",
                  "size": "796", "price": "156.490", "lossGain": "0", "fee": "3",
                  "timestamp": "2026-07-07T00:00:01Z" }
            ] }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let status = api.stop_order_status("USD_JPY", "778899").await.unwrap();
    match status {
        StopOrderStatus::Executed { price, commission } => {
            assert_eq!(price, dec!(156.500));
            assert_eq!(commission, dec!(6));
        }
        other => panic!("expected Executed, got {other:?}"),
    }
}

#[tokio::test]
async fn stop_order_status_executed_falls_back_to_order_price_when_executions_are_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private/v1/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": { "list": [{
                "orderId": 778899, "symbol": "USD_JPY", "side": "SELL",
                "executionType": "STOP", "size": "1592", "executedSize": "1592",
                "price": "156.500", "status": "EXECUTED",
                "timestamp": "2026-07-07T00:00:00Z"
            }] }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/private/v1/executions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": 0,
            "data": { "list": [] }
        })))
        .mount(&server)
        .await;
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let status = api.stop_order_status("USD_JPY", "778899").await.unwrap();
    match status {
        StopOrderStatus::Executed { price, commission } => {
            assert_eq!(price, dec!(156.500));
            assert_eq!(commission, Decimal::ZERO);
        }
        other => panic!("expected Executed, got {other:?}"),
    }
}

fn market_order(price: Option<Decimal>, close_position_id: Option<&str>) -> SendChildOrderRequest {
    SendChildOrderRequest {
        product_code: "USD_JPY".into(),
        child_order_type: ChildOrderType::Market,
        side: Side::Buy,
        size: dec!(1000),
        price,
        minute_to_expire: None,
        time_in_force: None,
        close_position_id: close_position_id.map(str::to_string),
    }
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
    let api = GmoFxPrivateApi::new("k".into(), "s".into());
    for (req, expected) in cases {
        let err = api.send_child_order(req).await.unwrap_err();
        assert!(err.to_string().contains(expected), "err={err}");
    }
}

#[tokio::test]
async fn place_stop_order_rejects_non_numeric_position_id() {
    let api = GmoFxPrivateApi::new("k".into(), "s".into());
    let err = api
        .place_stop_order("USD_JPY", Side::Sell, dec!(1592), dec!(156.5), Some("abc"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("invalid position_id: not a u64"),
        "err={err}"
    );
}

#[tokio::test]
async fn cancel_child_order_rejects_non_numeric_id() {
    let api = GmoFxPrivateApi::new("k".into(), "s".into());
    let err = api.cancel_child_order("USD_JPY", "abc").await.unwrap_err();
    assert!(
        err.to_string()
            .contains("invalid child_order_acceptance_id: not a u64"),
        "err={err}"
    );
}

#[tokio::test]
async fn cancel_stop_order_rejects_non_numeric_id() {
    let api = GmoFxPrivateApi::new("k".into(), "s".into());
    let err = api.cancel_stop_order("USD_JPY", "abc").await.unwrap_err();
    assert!(
        err.to_string().contains("invalid stop_order_id: not a u64"),
        "err={err}"
    );
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
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
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
    let api = GmoFxPrivateApi::new("k".into(), "s".into()).with_api_url(server.uri());
    let err = api
        .send_child_order(market_order(None, None))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("failed to parse response body: "), "err={err}");
    assert!(msg.contains("not json"), "err={err}");
}
