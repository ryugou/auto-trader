use std::time::Duration;

use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mock of the Gemini `generateContent` API.
pub struct MockGemini {
    server: MockServer,
}

impl MockGemini {
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

    /// Replaces the routes with a response whose first candidate carries `json` as its text, for the parameter proposal of the weekly batch.
    pub async fn parameter_proposal(&self, json: &str) {
        self.server.reset().await;
        let body = gemini_response_wrapper(json);

        Mock::given(method("POST"))
            .and(path_regex(r"/v1beta/models/.+:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&self.server)
            .await;
    }

    /// Replaces the routes with a response whose first candidate carries `json` as its text, for the swing-trade decision.
    pub async fn swing_signal(&self, json: &str) {
        self.server.reset().await;
        let body = gemini_response_wrapper(json);

        Mock::given(method("POST"))
            .and(path_regex(r"/v1beta/models/.+:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&self.server)
            .await;
    }

    /// Replaces the routes with a response whose body is not valid JSON.
    pub async fn invalid_response(&self) {
        self.server.reset().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/v1beta/models/.+:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_string("this is not valid json {{{"))
            .mount(&self.server)
            .await;
    }

    /// Replaces the routes with a response that is delayed by 60 seconds.
    pub async fn timeout(&self) {
        self.server.reset().await;
        Mock::given(method("POST"))
            .and(path_regex(r"/v1beta/models/.+:generateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("{}")
                    .set_delay(Duration::from_secs(60)),
            )
            .mount(&self.server)
            .await;
    }
}

fn gemini_response_wrapper(text: &str) -> serde_json::Value {
    serde_json::json!({
        "candidates": [{
            "content": {
                "parts": [{"text": text}],
                "role": "model"
            },
            "finishReason": "STOP"
        }]
    })
}
