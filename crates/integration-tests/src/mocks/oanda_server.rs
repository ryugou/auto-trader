//! Wiremock-based mock for the OANDA REST API (candles endpoint mock).

use std::time::Duration;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub struct MockOandaServer {
    server: MockServer,
}

impl MockOandaServer {
    /// Start a new mock server (no routes mounted yet).
    pub async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    /// Base URL of the mock server.
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// Remove all previously mounted routes.
    pub async fn reset(&self) {
        self.server.reset().await;
    }

    /// Mount a candles response for any instrument.
    /// `candles_json` is the raw JSON array value for `"candles"`.
    pub async fn normal_candles(&self, candles_json: serde_json::Value) {
        self.reset().await;
        let body = serde_json::json!({
            "candles": candles_json,
        });

        Mock::given(method("GET"))
            .and(path_regex(r"/v3/accounts/.+/instruments/.+/candles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&self.server)
            .await;
    }

    /// Mount an HTTP error response on the candles endpoint.
    pub async fn http_error(&self, code: u16) {
        self.reset().await;
        Mock::given(method("GET"))
            .and(path_regex(r"/v3/accounts/.+/instruments/.+/candles"))
            .respond_with(ResponseTemplate::new(code))
            .mount(&self.server)
            .await;
    }

    /// Mount a delayed response that exceeds a typical client timeout.
    pub async fn timeout(&self) {
        self.reset().await;
        Mock::given(method("GET"))
            .and(path_regex(r"/v3/accounts/.+/instruments/.+/candles"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"candles": []}))
                    .set_delay(Duration::from_secs(60)),
            )
            .mount(&self.server)
            .await;
    }
}
