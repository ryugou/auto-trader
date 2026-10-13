use wiremock::MockServer;

use super::OandaPrivateApi;

pub(super) fn api_at(base_url: &str) -> OandaPrivateApi {
    OandaPrivateApi::new(
        base_url.to_string(),
        "101-001-12345-001".to_string(),
        "fake-token".to_string(),
    )
    .unwrap()
}

pub(super) fn api_for(server: &MockServer) -> OandaPrivateApi {
    api_at(&server.uri())
}
