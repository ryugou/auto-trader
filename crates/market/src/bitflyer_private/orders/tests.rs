use wiremock::{MockServer, ResponseTemplate};

use super::*;
use crate::bitflyer_private::test_support::{api_for, mount_get};

const PARENT_COMPLETED: &str =
    r#"{"parent_order_id":"JCP20260707-000000-111111","parent_order_state":"COMPLETED"}"#;
const PARENT_STATE_UNKNOWN: &str = r#"{"parent_order_id":"JCP20260707-000000-111111"}"#;
const COMPLETED_CHILD: &str = r#"[{
    "id": 1,
    "child_order_id": "JOR20260707-000000-222222",
    "product_code": "FX_BTC_JPY",
    "side": "SELL",
    "child_order_type": "MARKET",
    "price": "0",
    "average_price": "0",
    "size": "0.004",
    "child_order_state": "COMPLETED",
    "expire_date": "2026-07-14T00:00:00",
    "child_order_date": "2026-07-07T00:00:00",
    "child_order_acceptance_id": "JRF20260707-000000-333333",
    "outstanding_size": "0",
    "cancel_size": "0",
    "executed_size": "0.004",
    "total_commission": "0"
}]"#;

async fn stop_order_status_for(
    parent: ResponseTemplate,
    children: Option<&str>,
    executions: Option<&str>,
) -> Result<StopOrderStatus, BitflyerApiError> {
    let server = MockServer::start().await;
    mount_get(&server, "/v1/me/getparentorder", parent).await;
    if let Some(children) = children {
        mount_get(
            &server,
            "/v1/me/getchildorders",
            ResponseTemplate::new(200).set_body_string(children),
        )
        .await;
    }
    if let Some(executions) = executions {
        mount_get(
            &server,
            "/v1/me/getexecutions",
            ResponseTemplate::new(200).set_body_string(executions),
        )
        .await;
    }
    api_for(&server)
        .stop_order_status("FX_BTC_JPY", "JRF20260707-000000-000000")
        .await
}

#[tokio::test]
async fn stop_order_status_errors_instead_of_fabricating_zero_price_when_no_fill_data() {
    let err = stop_order_status_for(
        ResponseTemplate::new(200).set_body_string(PARENT_COMPLETED),
        Some(COMPLETED_CHILD),
        Some("[]"),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, BitflyerApiError::InvalidResponse(_)),
        "expected InvalidResponse, got: {err:?}"
    );
}

#[tokio::test]
async fn stop_order_status_resolves_gone_or_active_from_parent_and_children() {
    let cases = [
        (
            "completed parent with no executed child",
            ResponseTemplate::new(200).set_body_string(PARENT_COMPLETED),
            Some("[]"),
            StopOrderStatus::Gone,
        ),
        (
            "unknown parent state with no executed child",
            ResponseTemplate::new(200).set_body_string(PARENT_STATE_UNKNOWN),
            Some("[]"),
            StopOrderStatus::Active,
        ),
        (
            "parent order not found",
            ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "status": -208,
                "error_message": "no such order"
            })),
            None,
            StopOrderStatus::Gone,
        ),
    ];

    for (name, parent, children, expected) in cases {
        let status = stop_order_status_for(parent, children, None)
            .await
            .unwrap_or_else(|e| {
                panic!("case {name}: stop_order_status should succeed, got: {e:?}")
            });
        assert_eq!(status, expected, "case {name}");
    }
}
