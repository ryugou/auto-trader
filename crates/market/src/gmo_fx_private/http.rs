use std::num::NonZeroU32;
use std::sync::Arc;

use anyhow::Context as _;
use chrono::Utc;
use reqwest::{Client, Method};

use crate::bitflyer_private::truncate_body;

use super::types::GmoApiResponse;

/// Rate limiter that delays requests to keep them within a quota.
pub type RateLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

const DEFAULT_API_URL: &str = "https://forex-api.coin.z.com";

pub(crate) fn sign_path_for(path: &str) -> &str {
    path.split_once('?').map(|(p, _)| p).unwrap_or(path)
}

pub(crate) fn sign(
    api_secret: &str,
    timestamp_ms: i64,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    crate::bitflyer_private::sign(api_secret, &timestamp_ms.to_string(), method, path, body)
}

/// Client of the GMO FX private HTTP API that signs each request with the API key and secret.
pub struct GmoFxPrivateApi {
    api_url: String,
    api_key: String,
    api_secret: String,
    http: Client,
    post_limiter: Arc<RateLimiter>,
    get_limiter: Arc<RateLimiter>,
}

const POST_RATE_LIMIT_PER_SECOND: NonZeroU32 = match NonZeroU32::new(1) {
    Some(n) => n,
    None => panic!("invalid post rate limit per second: zero"),
};
const GET_RATE_LIMIT_PER_SECOND: NonZeroU32 = match NonZeroU32::new(6) {
    Some(n) => n,
    None => panic!("invalid get rate limit per second: zero"),
};

impl GmoFxPrivateApi {
    /// Creates a client of the API at `https://forex-api.coin.z.com` that limits POST requests to 1 per second and GET requests to 6 per second.
    ///
    /// # Errors
    ///
    /// - Returns an error if the HTTP client cannot be built.
    pub fn new(api_key: String, api_secret: String) -> anyhow::Result<Self> {
        let post_quota = governor::Quota::per_second(POST_RATE_LIMIT_PER_SECOND)
            .allow_burst(POST_RATE_LIMIT_PER_SECOND);
        let get_quota = governor::Quota::per_second(GET_RATE_LIMIT_PER_SECOND)
            .allow_burst(GET_RATE_LIMIT_PER_SECOND);
        Ok(Self {
            api_url: DEFAULT_API_URL.to_string(),
            api_key,
            api_secret,
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .context("failed to build reqwest client")?,
            post_limiter: Arc::new(RateLimiter::direct(post_quota)),
            get_limiter: Arc::new(RateLimiter::direct(get_quota)),
        })
    }

    /// Replaces the base URL of the API with `url`.
    pub fn with_api_url(mut self, url: String) -> Self {
        self.api_url = url;
        self
    }

    pub(super) async fn signed_request<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> anyhow::Result<T> {
        let is_post = method == Method::POST;
        if is_post {
            self.post_limiter.until_ready().await;
        } else {
            self.get_limiter.until_ready().await;
        }
        let body_str = body.map(|v| v.to_string()).unwrap_or_default();
        let ts = Utc::now().timestamp_millis();
        let sign_path = sign_path_for(path);
        let sig = sign(&self.api_secret, ts, method.as_str(), sign_path, &body_str);
        let url = format!("{}{}{}", self.api_url, "/private", path);
        let mut req = self
            .http
            .request(method.clone(), &url)
            .header("API-KEY", &self.api_key)
            .header("API-TIMESTAMP", ts.to_string())
            .header("API-SIGN", sig);
        if is_post {
            req = req
                .header("Content-Type", "application/json")
                .body(body_str.clone());
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("failed to send http request to {method} {url}"))?;
        let status = resp.status();
        let text = resp.text().await.context("failed to read response body")?;
        if !status.is_success() {
            anyhow::bail!(
                "failed to call gmo api: status={status} body={}",
                truncate_body(&text)
            );
        }
        let api_resp: GmoApiResponse<T> = serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse response body: {e}: {}",
                truncate_body(&text)
            )
        })?;
        if api_resp.status != 0 {
            anyhow::bail!(
                "failed to call gmo api: status={} messages={:?}",
                api_resp.status,
                api_resp.messages
            );
        }
        api_resp.data.ok_or_else(|| {
            anyhow::anyhow!("invalid response: data is null: {}", truncate_body(&text))
        })
    }
}

#[cfg(test)]
mod tests;
