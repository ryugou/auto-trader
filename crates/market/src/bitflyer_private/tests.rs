use rust_decimal_macros::dec;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn api_with(base_url: &str, key: &str, secret: &str) -> BitflyerPrivateApi {
    BitflyerPrivateApi::new_for_test(base_url.to_string(), key.to_string(), secret.to_string())
}

fn api_for(server: &MockServer) -> BitflyerPrivateApi {
    api_with(&server.uri(), "key", "secret")
}

async fn mount_get(server: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(route))
        .respond_with(response)
        .mount(server)
        .await;
}

#[test]
fn sign_matches_known_hmac_sha256_vector() {
    let sig = sign(
        "test_secret",
        "1234567890",
        "GET",
        "/v1/me/getpermissions",
        "",
    );
    assert_eq!(
        sig,
        "444e09cb91f6fd945ce0d21fa047e8c66d1fdf87c047c26f39d968c7352cd5c0"
    );
}

#[test]
fn sign_includes_post_body() {
    let with_body = sign(
        "secret",
        "1000",
        "POST",
        "/v1/me/sendchildorder",
        r#"{"product_code":"FX_BTC_JPY"}"#,
    );
    let without_body = sign("secret", "1000", "POST", "/v1/me/sendchildorder", "");
    assert_ne!(with_body, without_body, "body must affect the signature");
}

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
fn auth_headers_contain_key_timestamp_and_sign() {
    let api = api_with("http://example.invalid", "test-key", "test-secret");
    let headers = api.auth_headers("1234567890", "GET", "/v1/me/getcollateral", "");
    assert_eq!(headers.get("ACCESS-KEY").unwrap(), "test-key");
    assert_eq!(headers.get("ACCESS-TIMESTAMP").unwrap(), "1234567890");
    let expected = sign(
        "test-secret",
        "1234567890",
        "GET",
        "/v1/me/getcollateral",
        "",
    );
    assert_eq!(headers.get("ACCESS-SIGN").unwrap(), &expected);
}

#[test]
fn debug_redacts_api_key_and_secret() {
    let api = api_with("http://example.invalid", "my-super-key", "my-super-secret");
    let dbg = format!("{:?}", api);
    assert!(
        !dbg.contains("my-super-key"),
        "api_key must not appear in Debug output, got: {dbg}"
    );
    assert!(
        !dbg.contains("my-super-secret"),
        "api_secret must not appear in Debug output, got: {dbg}"
    );
    assert!(
        dbg.contains("***redacted***"),
        "Debug output should contain '***redacted***', got: {dbg}"
    );
}

#[test]
fn truncate_body_caps_at_512_chars() {
    let long = "a".repeat(1000);
    let truncated = truncate_body(&long);
    assert_eq!(truncated.len(), 512, "should be exactly 512 chars");
}

#[test]
fn truncate_body_passthrough_for_short_text() {
    let short = "hello";
    assert_eq!(truncate_body(short), "hello");
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

fn position_json(side: &str, open_date: &str, sfd: &str) -> serde_json::Value {
    serde_json::json!({
        "product_code": "FX_BTC_JPY",
        "side": side,
        "price": "36000",
        "size": "0.1",
        "commission": "0",
        "swap_point_accumulate": "0",
        "require_collateral": "120000",
        "open_date": open_date,
        "leverage": "2",
        "pnl": "0",
        "sfd": sfd
    })
}

async fn server_with_positions(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/v1/me/getpositions"))
        .and(wiremock::matchers::query_param(
            "product_code",
            "FX_BTC_JPY",
        ))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

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
async fn trait_fetch_close_sfd_delegates_to_inherent() {
    use crate::exchange_api::ExchangeApi as ExchangeApiTrait;

    let body = serde_json::json!([position_json("SELL", "2026-05-17T10:00:00", "42")]);
    let server = server_with_positions(ResponseTemplate::new(200).set_body_json(body)).await;

    let api: Box<dyn ExchangeApiTrait> = Box::new(api_for(&server));
    let sfd = api.fetch_close_sfd("FX_BTC_JPY").await.unwrap();
    assert_eq!(sfd, dec!(42));
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

async fn get_collateral_error(response: ResponseTemplate) -> BitflyerApiError {
    let server = collateral_server(response).await;
    api_for(&server)
        .request("GET", "/v1/me/getcollateral", "")
        .await
        .unwrap_err()
}

#[tokio::test]
async fn invalid_response_body_is_truncated() {
    let long_body: String = "x".repeat(2000);
    let err = get_collateral_error(ResponseTemplate::new(503).set_body_string(long_body)).await;

    let displayed = err.to_string();
    assert!(
        displayed.len() < 700,
        "error message must be truncated (< 700 chars), got len={}",
        displayed.len()
    );
    assert!(
        displayed.contains(&"x".repeat(512)),
        "first 512 chars of body must appear in message"
    );
    assert!(
        !displayed.contains(&"x".repeat(513)),
        "body beyond 512 chars must not appear in message"
    );
}

#[tokio::test]
async fn rate_limited_captures_retry_after() {
    let err = get_collateral_error(
        ResponseTemplate::new(429)
            .insert_header("Retry-After", "5")
            .set_body_string("rate limited"),
    )
    .await;

    match err {
        BitflyerApiError::RateLimited { retry_after } => {
            assert_eq!(
                retry_after,
                Some(std::time::Duration::from_secs(5)),
                "should capture Retry-After: 5"
            );
        }
        other => panic!("expected RateLimited, got {:?}", other),
    }
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

#[tokio::test]
async fn http_error_display_does_not_contain_url() {
    let api = api_with("http://127.0.0.1:1", "key", "secret");
    let err = api
        .request("GET", "/v1/me/getcollateral", "")
        .await
        .unwrap_err();

    assert!(
        matches!(err, BitflyerApiError::Http(_)),
        "expected Http variant, got: {err:?}"
    );
    let displayed = err.to_string();
    assert!(
        !displayed.contains("127.0.0.1"),
        "error message must not contain the request URL, got: {displayed}"
    );
}

#[tokio::test]
async fn request_treats_2xx_with_negative_status_body_as_error() {
    let err = get_collateral_error(ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "status": -1,
        "error_message": "unexpected error",
        "data": null
    })))
    .await;

    assert!(
        matches!(err, BitflyerApiError::ApiError { status: -1, .. }),
        "expected ApiError{{status: -1, ..}}, got: {err:?}"
    );
}

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
