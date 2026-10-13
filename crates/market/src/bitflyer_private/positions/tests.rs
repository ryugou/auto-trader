use rust_decimal_macros::dec;
use wiremock::{MockServer, ResponseTemplate};

use super::*;
use crate::bitflyer_private::test_support::{
    api_for, mount_get, position_json, server_with_positions,
};

#[tokio::test]
async fn fetch_close_sfd_sums_all_positions_for_product() {
    let body = serde_json::json!([
        position_json("BUY", "2026-05-17T10:00:00", "100"),
        position_json("BUY", "2026-05-17T10:01:00", "50"),
    ]);
    let server = server_with_positions(ResponseTemplate::new(200).set_body_json(body)).await;

    let sfd = api_for(&server)
        .fetch_close_sfd("FX_BTC_JPY")
        .await
        .unwrap();
    assert_eq!(
        sfd,
        dec!(150),
        "sfd values across all matching positions sum"
    );
}

#[tokio::test]
async fn fetch_close_sfd_returns_zero_for_empty_positions() {
    let server =
        server_with_positions(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .await;

    let sfd = api_for(&server)
        .fetch_close_sfd("FX_BTC_JPY")
        .await
        .unwrap();
    assert_eq!(sfd, Decimal::ZERO);
}

#[tokio::test]
async fn fetch_close_sfd_propagates_http_error() {
    let server = server_with_positions(ResponseTemplate::new(503).set_body_string("boom")).await;

    let res = api_for(&server).fetch_close_sfd("FX_BTC_JPY").await;
    assert!(res.is_err(), "503 must surface as Err");
}

async fn collateral_server(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    mount_get(&server, "/v1/me/getcollateral", response).await;
    server
}

#[tokio::test]
async fn parse_failure_error_message_is_truncated_to_512() {
    let garbage_body: String = "g".repeat(600);
    let server = collateral_server(ResponseTemplate::new(200).set_body_string(garbage_body)).await;

    let err = api_for(&server).get_collateral().await.unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains(&"g".repeat(512)),
        "first 512 chars must appear in error message"
    );
    assert!(
        !msg.contains(&"g".repeat(513)),
        "body beyond 512 chars must not appear in error message"
    );
}
