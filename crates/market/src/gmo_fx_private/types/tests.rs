use super::*;

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
