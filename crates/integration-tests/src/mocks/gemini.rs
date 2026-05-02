//! Wiremock-based mock for the Google Gemini generateContent API.

use std::time::Duration;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub struct MockGemini {
    server: MockServer,
}

impl MockGemini {
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

    /// Mount a response that returns a parameter proposal (weekly batch).
    ///
    /// `json` is the raw JSON text that Gemini would return inside the
    /// `candidates[0].content.parts[0].text` field (i.e. a serialized
    /// `GeminiProposal`).
    pub async fn parameter_proposal(&self, json: &str) {
        let body = gemini_response_wrapper(json);

        Mock::given(method("POST"))
            .and(path_regex(r"/v1beta/models/.+:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&self.server)
            .await;
    }

    /// Mount a response that returns a swing-trade signal decision.
    ///
    /// `json` is the raw JSON text for the trade decision, e.g.
    /// `{"action":"long","confidence":0.8,"sl_pips":50,"tp_pips":100,"reason":"..."}`
    pub async fn swing_signal(&self, json: &str) {
        let body = gemini_response_wrapper(json);

        Mock::given(method("POST"))
            .and(path_regex(r"/v1beta/models/.+:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&self.server)
            .await;
    }

    /// Mount a response that returns malformed JSON (not valid Gemini structure).
    pub async fn invalid_response(&self) {
        Mock::given(method("POST"))
            .and(path_regex(r"/v1beta/models/.+:generateContent"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("this is not valid json {{{"),
            )
            .mount(&self.server)
            .await;
    }

    /// Mount a response that delays long enough to trigger a client timeout.
    pub async fn timeout(&self) {
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

/// Wrap raw text in the standard Gemini generateContent response envelope.
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
