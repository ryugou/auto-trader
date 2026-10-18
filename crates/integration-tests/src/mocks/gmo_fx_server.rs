use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mock of the ticker endpoint of the GMO FX public API.
pub struct MockGmoFxServer {
    server: MockServer,
}

impl MockGmoFxServer {
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

    /// Replaces the routes with a ticker response of status 0 that lists each symbol of `pairs` as `OPEN`.
    pub async fn normal_ticker(&self, pairs: &[&str]) {
        self.server.reset().await;
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

    /// Replaces the routes with a ticker response of status 5 that carries the message `MAINTENANCE`.
    pub async fn maintenance(&self) {
        self.server.reset().await;
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

    /// Replaces the routes with a ticker response of status 0 that lists each symbol of `pairs` as `CLOSE`.
    pub async fn market_closed(&self, pairs: &[&str]) {
        self.server.reset().await;
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

    /// Replaces the routes with a ticker response that has the HTTP status `code` and no body.
    pub async fn http_error(&self, code: u16) {
        self.server.reset().await;
        Mock::given(method("GET"))
            .and(path("/public/v1/ticker"))
            .respond_with(ResponseTemplate::new(code))
            .mount(&self.server)
            .await;
    }
}
