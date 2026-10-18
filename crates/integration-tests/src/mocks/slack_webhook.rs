use std::sync::{Arc, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::lock_mock_state;

/// POST request that the mock received, together with the status it answered with.
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    /// Body of the request.
    pub body: String,
    /// HTTP status of the response to the request.
    pub status: u16,
}

#[derive(Clone)]
struct CapturingResponder {
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    error_code: Option<u16>,
}

impl Respond for CapturingResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&request.body).to_string();
        let status = self.error_code.unwrap_or(200);
        match lock_mock_state(&self.requests, "requests") {
            Ok(mut requests) => requests.push(CapturedRequest { body, status }),
            Err(e) => return ResponseTemplate::new(500).set_body_string(e.to_string()),
        }
        if let Some(code) = self.error_code {
            ResponseTemplate::new(code)
        } else {
            ResponseTemplate::new(200).set_body_string("ok")
        }
    }
}

/// Mock of a Slack webhook that records the POST requests it receives to `/webhook`.
pub struct MockSlackWebhook {
    server: MockServer,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl MockSlackWebhook {
    /// Starts the mock, which answers with status 200, and returns it together with the URL of its webhook.
    pub async fn start() -> (Self, String) {
        let server = MockServer::start().await;
        let requests: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let url = format!("{}/webhook", server.uri());
        let this = Self { server, requests };
        this.mount_responder(None).await;
        (this, url)
    }

    /// Returns the base URL of the server.
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// Returns the bodies of the captured requests, in the order they arrived.
    ///
    /// # Errors
    ///
    /// - Returns an error if the lock on the captured requests is poisoned.
    pub fn captured_bodies(&self) -> anyhow::Result<Vec<String>> {
        let requests = lock_mock_state(&self.requests, "requests")?;
        Ok(requests.iter().map(|r| r.body.clone()).collect())
    }

    /// Returns the captured requests with the status of each response, in the order they arrived.
    ///
    /// # Errors
    ///
    /// - Returns an error if the lock on the captured requests is poisoned.
    pub fn captured_requests(&self) -> anyhow::Result<Vec<CapturedRequest>> {
        let requests = lock_mock_state(&self.requests, "requests")?;
        Ok(requests.clone())
    }

    /// Makes the webhook answer subsequent requests with the status `code`.
    pub async fn with_error_response(&self, code: u16) {
        self.mount_responder(Some(code)).await;
    }

    /// Makes the webhook answer subsequent requests with the status 200.
    pub async fn with_success_response(&self) {
        self.mount_responder(None).await;
    }

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
