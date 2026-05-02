//! Wiremock-based mock for the GMO Coin FX Public API ticker endpoint.

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub struct MockGmoFxServer {
    server: MockServer,
}

impl MockGmoFxServer {
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

    /// Mount a ticker response with status=0, OPEN symbols.
    pub async fn normal_ticker(&self, pairs: &[&str]) {
        let data: Vec<serde_json::Value> = pairs
            .iter()
            .map(|symbol| {
                serde_json::json!({
                    "symbol": symbol,
                    "ask": "150.123",
                    "bid": "150.100",
                    "timestamp": "2026-04-29T12:00:00.000Z",
                    "status": "OPEN"
                })
            })
            .collect();

        let body = serde_json::json!({
            "status": 0,
            "data": data,
        });

        Mock::given(method("GET"))
            .and(path("/public/v1/ticker"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&self.server)
            .await;
    }

    /// Mount a maintenance response (status=5).
    pub async fn maintenance(&self) {
        let body = serde_json::json!({
            "status": 5,
            "messages": [{
                "message_code": "ERR-5201",
                "message_string": "MAINTENANCE"
            }]
        });

        Mock::given(method("GET"))
            .and(path("/public/v1/ticker"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&self.server)
            .await;
    }

    /// Mount a response where all pairs have status="CLOSE".
    pub async fn market_closed(&self, pairs: &[&str]) {
        let data: Vec<serde_json::Value> = pairs
            .iter()
            .map(|symbol| {
                serde_json::json!({
                    "symbol": symbol,
                    "ask": "150.123",
                    "bid": "150.100",
                    "timestamp": "2026-04-29T12:00:00.000Z",
                    "status": "CLOSE"
                })
            })
            .collect();

        let body = serde_json::json!({
            "status": 0,
            "data": data,
        });

        Mock::given(method("GET"))
            .and(path("/public/v1/ticker"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&self.server)
            .await;
    }

    /// Mount an HTTP error response.
    pub async fn http_error(&self, code: u16) {
        Mock::given(method("GET"))
            .and(path("/public/v1/ticker"))
            .respond_with(ResponseTemplate::new(code))
            .mount(&self.server)
            .await;
    }
}
