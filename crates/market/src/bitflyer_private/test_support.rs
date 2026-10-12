use wiremock::{Mock, MockServer, ResponseTemplate};

use super::BitflyerPrivateApi;

pub(super) fn api_with(base_url: &str, key: &str, secret: &str) -> BitflyerPrivateApi {
    BitflyerPrivateApi::new_for_test(base_url.to_string(), key.to_string(), secret.to_string())
        .unwrap()
}

pub(super) fn api_for(server: &MockServer) -> BitflyerPrivateApi {
    api_with(&server.uri(), "key", "secret")
}

pub(super) async fn mount_get(server: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(route))
        .respond_with(response)
        .mount(server)
        .await;
}

pub(super) fn position_json(side: &str, open_date: &str, sfd: &str) -> serde_json::Value {
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

pub(super) async fn server_with_positions(response: ResponseTemplate) -> MockServer {
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
