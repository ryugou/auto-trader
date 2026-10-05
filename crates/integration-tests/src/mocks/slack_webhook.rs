//! Wiremock-based mock for a Slack incoming-webhook endpoint.
//!
//! Captures all POST request bodies so tests can assert on them.

use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// One captured POST request: the body text and the HTTP status this mock
/// responded with to that specific request. Kept together (rather than as
/// two parallel `Vec`s) so callers checking "which status did the request
/// with this body get" cannot get the two lists out of sync.
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub body: String,
    pub status: u16,
}

/// A responder that captures request bodies and returns 200.
#[derive(Clone)]
struct CapturingResponder {
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    error_code: Option<u16>,
}

impl Respond for CapturingResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&request.body).to_string();
        let status = self.error_code.unwrap_or(200);
        self.requests
            .lock()
            .unwrap()
            .push(CapturedRequest { body, status });
        if let Some(code) = self.error_code {
            ResponseTemplate::new(code)
        } else {
            ResponseTemplate::new(200).set_body_string("ok")
        }
    }
}

pub struct MockSlackWebhook {
    server: MockServer,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl MockSlackWebhook {
    /// Start the mock webhook server. Returns `(Self, webhook_url)`.
    pub async fn start() -> (Self, String) {
        let server = MockServer::start().await;
        let requests: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let url = format!("{}/webhook", server.uri());
        let this = Self { server, requests };
        this.mount_responder(None).await;
        (this, url)
    }

    /// Base URL of the mock server.
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// Return all captured POST bodies.
    pub fn captured_bodies(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.body.clone())
            .collect()
    }

    /// Return all captured requests, each paired with the HTTP status this
    /// mock responded with. Use this (instead of `captured_bodies`) when a
    /// test needs to distinguish a request that got a failure response from
    /// one that got a success response, e.g. to confirm a retry eventually
    /// succeeded rather than merely being re-attempted.
    pub fn captured_requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Replace the mounted mock so subsequent requests return an error.
    /// Use [`Self::with_success_response`] to switch back (e.g. to simulate
    /// a transient webhook outage that later recovers).
    pub async fn with_error_response(&self, code: u16) {
        self.mount_responder(Some(code)).await;
    }

    /// Replace the mounted mock so subsequent requests succeed again.
    pub async fn with_success_response(&self) {
        self.mount_responder(None).await;
    }

    /// Reset existing mocks and mount a responder with the given behavior.
    /// `error_code: None` means respond 200; `Some(code)` means respond with
    /// that status. Shared by `start`/`with_error_response`/
    /// `with_success_response` so the mount logic lives in one place.
    async fn mount_responder(&self, error_code: Option<u16>) {
        self.server.reset().await;

        let responder = CapturingResponder {
            requests: Arc::clone(&self.requests),
            error_code,
        };

        Mock::given(method("POST"))
            .and(path("/webhook"))
            .respond_with(responder)
            .mount(&self.server)
            .await;
    }
}
