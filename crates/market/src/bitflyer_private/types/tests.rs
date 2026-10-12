use rust_decimal_macros::dec;

use super::*;

#[test]
fn deserialize_send_child_order_response() {
    let json = r#"{"child_order_acceptance_id":"JRF20150707-050237-639234"}"#;
    let r: SendChildOrderResponse = serde_json::from_str(json).unwrap();
    assert_eq!(r.child_order_acceptance_id, "JRF20150707-050237-639234");
}

#[test]
fn deserialize_child_order() {
    let json = r#"{
            "id": 138398,
            "child_order_id": "JOR20150707-084555-022523",
            "product_code": "BTC_JPY",
            "side": "BUY",
            "child_order_type": "LIMIT",
            "price": "30000",
            "average_price": "30000",
            "size": "0.1",
            "child_order_state": "COMPLETED",
            "expire_date": "2015-07-14T07:25:47",
            "child_order_date": "2015-07-07T08:45:47",
            "child_order_acceptance_id": "JRF20150707-084555-022523",
            "outstanding_size": "0",
            "cancel_size": "0",
            "executed_size": "0.1",
            "total_commission": "0"
        }"#;
    let o: ChildOrder = serde_json::from_str(json).unwrap();
    assert_eq!(o.child_order_id, "JOR20150707-084555-022523");
    assert_eq!(o.side, "BUY");
    assert_eq!(o.child_order_state, ChildOrderState::Completed);
    assert_eq!(o.size, dec!(0.1));
}

#[test]
fn deserialize_execution() {
    let json = r#"{
            "id": 37233,
            "child_order_id": "JOR20150707-084555-022523",
            "side": "BUY",
            "price": "30000",
            "size": "0.1",
            "commission": "0",
            "exec_date": "2015-07-07T09:57:40.397",
            "child_order_acceptance_id": "JRF20150707-084555-022523"
        }"#;
    let e: Execution = serde_json::from_str(json).unwrap();
    assert_eq!(e.id, 37233);
    assert_eq!(e.size, dec!(0.1));
}

#[test]
fn deserialize_collateral() {
    let json = r#"{
            "collateral": "100000",
            "open_position_pnl": "-715",
            "require_collateral": "19857",
            "keep_rate": "5.0"
        }"#;
    let c: Collateral = serde_json::from_str(json).unwrap();
    assert_eq!(c.collateral, dec!(100000));
    assert_eq!(c.open_position_pnl, dec!(-715));
}

#[test]
fn deserialize_exchange_position() {
    let json = r#"{
            "product_code": "FX_BTC_JPY",
            "side": "BUY",
            "price": "36000",
            "size": "10",
            "commission": "0",
            "swap_point_accumulate": "-35",
            "require_collateral": "120000",
            "open_date": "2015-11-03T10:04:45.011",
            "leverage": "3",
            "pnl": "965",
            "sfd": "0"
        }"#;
    let p: ExchangePosition = serde_json::from_str(json).unwrap();
    assert_eq!(p.product_code, "FX_BTC_JPY");
    assert_eq!(p.size, dec!(10));
}

#[test]
fn deserialize_bitflyer_api_error() {
    let json = r#"{"status": -205, "error_message": "Insufficient fund", "data": null}"#;
    let e: BitflyerErrorBody = serde_json::from_str(json).unwrap();
    assert_eq!(e.status, -205);
    assert_eq!(e.error_message, "Insufficient fund");
}

#[test]
fn send_child_order_request_omits_close_position_id_on_the_wire() {
    let req = SendChildOrderRequest {
        product_code: "FX_BTC_JPY".to_string(),
        child_order_type: ChildOrderType::Market,
        side: Side::Buy,
        size: dec!(0.1),
        price: None,
        minute_to_expire: None,
        time_in_force: None,
        close_position_id: Some("internal-position-id".to_string()),
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(
        !json.contains("close_position_id"),
        "close_position_id must never appear on the wire, got: {json}"
    );
}
