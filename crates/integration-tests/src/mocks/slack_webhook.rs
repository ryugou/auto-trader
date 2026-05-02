//! Wiremock-based mock for a Slack incoming-webhook endpoint.
//!
//! Captures all POST request bodies so tests can assert on them.

use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A responder that captures request bodies and returns 200.
#[derive(Clone)]
struct CapturingResponder {
    bodies: Arc<Mutex<Vec<String>>>,
    error_code: Option<u16>,
}

impl Respond for CapturingResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&request.body).to_string();
        self.bodies.lock().unwrap().push(body);
        if let Some(code) = self.error_code {
            ResponseTemplate::new(code)
        } else {
            ResponseTemplate::new(200).set_body_string("ok")
        }
    }
}

pub struct MockSlackWebhook {
    server: MockServer,
    bodies: Arc<Mutex<Vec<String>>>,
}

impl MockSlackWebhook {
    /// Start the mock webhook server. Returns `(Self, webhook_url)`.
    pub async fn start() -> (Self, String) {
        let server = MockServer::start().await;
        let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let responder = CapturingResponder {
            bodies: Arc::clone(&bodies),
            error_code: None,
        };

        Mock::given(method("POST"))
            .and(path("/webhook"))
            .respond_with(responder)
            .mount(&server)
            .await;

        let url = format!("{}/webhook", server.uri());
        (Self { server, bodies }, url)
    }

    /// Base URL of the mock server.
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// Return all captured POST bodies.
    pub fn captured_bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap().clone()
    }

    /// Replace the mounted mock so subsequent requests return an error.
    pub async fn with_error_response(&self, code: u16) {
        // Reset existing mocks and mount the error variant.
        self.server.reset().await;

        let responder = CapturingResponder {
            bodies: Arc::clone(&self.bodies),
            error_code: Some(code),
        };

        Mock::given(method("POST"))
            .and(path("/webhook"))
            .respond_with(responder)
            .mount(&self.server)
            .await;
    }
}
