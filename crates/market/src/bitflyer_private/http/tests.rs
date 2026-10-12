use wiremock::{MockServer, ResponseTemplate};

use super::*;
use crate::bitflyer_private::test_support::{api_for, api_with, mount_get};

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
