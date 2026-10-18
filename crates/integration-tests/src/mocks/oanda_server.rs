use std::time::Duration;

use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mock of the candles endpoint of the OANDA API.
pub struct MockOandaServer {
    server: MockServer,
}

impl MockOandaServer {
    /// Starts a mock server that has no routes.
    pub async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    /// Returns the base URL of the server.
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// Removes all routes.
    pub async fn reset(&self) {
        self.server.reset().await;
    }

    /// Replaces the routes with a candles response for any account and instrument whose `candles` field is `candles_json`.
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

    /// Replaces the routes with a candles response that has the HTTP status `code` and no body.
    pub async fn http_error(&self, code: u16) {
        self.reset().await;
        Mock::given(method("GET"))
            .and(path_regex(r"/v3/accounts/.+/instruments/.+/candles"))
            .respond_with(ResponseTemplate::new(code))
            .mount(&self.server)
            .await;
    }

    /// Replaces the routes with a candles response that holds no candles and is delayed by 60 seconds.
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
