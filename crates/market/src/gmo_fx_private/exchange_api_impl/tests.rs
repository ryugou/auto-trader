use rust_decimal_macros::dec;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::gmo_fx_private::test_support::{api_for, offline_api};

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
    let api = api_for(&server);
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
    let api = api_for(&server);
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
    let api = api_for(&server);
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
async fn requires_close_position_id_is_true() {
    let api = offline_api();
    assert!(api.requires_close_position_id());
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
    let api = api_for(&server);
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
    let api = api_for(&server);
    api.cancel_child_order("USD_JPY", "54321").await.unwrap();
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
    let api = api_for(&server);
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
    let api = offline_api();
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
    let api = api_for(&server);
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
    let api = api_for(&server);
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
    let api = api_for(&server);
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
    let api = api_for(&server);
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
    let api = api_for(&server);
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
    let api = api_for(&server);
    let status = api.stop_order_status("USD_JPY", "778899").await.unwrap();
    match status {
        StopOrderStatus::Executed { price, commission } => {
            assert_eq!(price, dec!(156.500));
            assert_eq!(commission, Decimal::ZERO);
        }
        other => panic!("expected Executed, got {other:?}"),
    }
}

#[tokio::test]
async fn place_stop_order_rejects_non_numeric_position_id() {
    let api = offline_api();
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
    let api = offline_api();
    let err = api.cancel_child_order("USD_JPY", "abc").await.unwrap_err();
    assert!(
        err.to_string()
            .contains("invalid child_order_acceptance_id: not a u64"),
        "err={err}"
    );
}

#[tokio::test]
async fn cancel_stop_order_rejects_non_numeric_id() {
    let api = offline_api();
    let err = api.cancel_stop_order("USD_JPY", "abc").await.unwrap_err();
    assert!(
        err.to_string().contains("invalid stop_order_id: not a u64"),
        "err={err}"
    );
}
