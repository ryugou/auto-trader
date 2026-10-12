use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use anyhow::Context as _;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::types::{BitflyerApiError, BitflyerErrorBody};

type HmacSha256 = Hmac<Sha256>;

pub type RateLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

#[derive(Clone)]
pub struct BitflyerPrivateApi {
    base_url: String,
    api_key: String,
    api_secret: String,
    http: reqwest::Client,
    rate_limiter: Option<Arc<RateLimiter>>,
}

impl std::fmt::Debug for BitflyerPrivateApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitflyerPrivateApi")
            .field("base_url", &self.base_url)
            .field("api_key", &"***redacted***")
            .field("api_secret", &"***redacted***")
            .field(
                "rate_limiter",
                if self.rate_limiter.is_some() {
                    &"<enabled>"
                } else {
                    &"<none>"
                },
            )
            .finish_non_exhaustive()
    }
}

impl BitflyerPrivateApi {
    pub fn new(api_key: String, api_secret: String) -> anyhow::Result<Self> {
        const CONSERVATIVE_RATE_LIMIT_REQUESTS_PER_MINUTE: NonZeroU32 = match NonZeroU32::new(30) {
            Some(n) => n,
            None => panic!("invalid rate limit requests per minute: zero"),
        };
        const CONSERVATIVE_RATE_LIMIT_BURST: NonZeroU32 = match NonZeroU32::new(10) {
            Some(n) => n,
            None => panic!("invalid rate limit burst: zero"),
        };
        let limiter = Arc::new(governor::RateLimiter::direct(
            governor::Quota::per_minute(CONSERVATIVE_RATE_LIMIT_REQUESTS_PER_MINUTE)
                .allow_burst(CONSERVATIVE_RATE_LIMIT_BURST),
        ));
        let mut api =
            Self::with_base_url("https://api.bitflyer.com".to_string(), api_key, api_secret)?;
        api.rate_limiter = Some(limiter);
        Ok(api)
    }

    pub(crate) fn with_base_url(
        base_url: String,
        api_key: String,
        api_secret: String,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            base_url,
            api_key,
            api_secret,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .context("failed to build reqwest client")?,
            rate_limiter: None,
        })
    }

    #[doc(hidden)]
    pub fn new_for_test(
        base_url: String,
        api_key: String,
        api_secret: String,
    ) -> anyhow::Result<Self> {
        Self::with_base_url(base_url, api_key, api_secret)
    }

    #[doc(hidden)]
    pub fn with_rate_limiter(mut self, limiter: Arc<RateLimiter>) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    pub(crate) fn auth_headers(
        &self,
        timestamp: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> HashMap<&'static str, String> {
        let sig = sign(&self.api_secret, timestamp, method, path, body);
        let mut h = HashMap::new();
        h.insert("ACCESS-KEY", self.api_key.clone());
        h.insert("ACCESS-TIMESTAMP", timestamp.to_string());
        h.insert("ACCESS-SIGN", sig);
        h
    }

    fn current_timestamp() -> String {
        Utc::now().timestamp().to_string()
    }

    pub(crate) async fn request(
        &self,
        method: &str,
        path: &str,
        body_json: &str,
    ) -> Result<String, BitflyerApiError> {
        if let Some(limiter) = &self.rate_limiter {
            limiter.until_ready().await;
        }
        let url = format!("{}{}", self.base_url, path);
        let ts = Self::current_timestamp();
        let headers = self.auth_headers(&ts, method, path, body_json);

        let mut req = match method {
            "GET" => self.http.get(&url),
            "POST" => self.http.post(&url),
            _ => {
                return Err(BitflyerApiError::InvalidResponse(format!(
                    "unsupported method: {method}"
                )));
            }
        };
        for (k, v) in &headers {
            req = req.header(*k, v);
        }
        if method == "POST" {
            req = req
                .header("Content-Type", "application/json")
                .body(body_json.to_string());
        }

        let resp = req.send().await?;
        let status = resp.status();

        if status.as_u16() == 429 {
            let retry_after = parse_retry_after(&resp);
            let _ = resp.text().await;
            return Err(BitflyerApiError::RateLimited { retry_after });
        }

        let text = resp.text().await.map_err(|e| {
            tracing::warn!(method, path, "failed to read response body");
            BitflyerApiError::Http(e.without_url())
        })?;

        if !status.is_success() {
            return match serde_json::from_str::<BitflyerErrorBody>(&text) {
                Ok(body) => Err(BitflyerApiError::from_body(body)),
                Err(_) => {
                    let truncated = truncate_body(&text);
                    tracing::warn!(
                        method,
                        path,
                        status = status.as_u16(),
                        "failed to parse non-2xx response body"
                    );
                    Err(BitflyerApiError::InvalidResponse(format!(
                        "failed to parse error response: non-2xx status {} body (truncated to {MAX_ERROR_BODY_CHARS} chars): {}",
                        status.as_u16(),
                        truncated
                    )))
                }
            };
        }

        if let Ok(body) = serde_json::from_str::<BitflyerErrorBody>(&text)
            && body.status < 0
        {
            return Err(BitflyerApiError::from_body(body));
        }

        Ok(text)
    }
}

pub(crate) const MAX_ERROR_BODY_CHARS: usize = 512;

pub(crate) fn truncate_body(text: &str) -> String {
    text.chars().take(MAX_ERROR_BODY_CHARS).collect()
}

fn parse_retry_after(resp: &reqwest::Response) -> Option<std::time::Duration> {
    let hdr = resp.headers().get("retry-after")?.to_str().ok()?;
    hdr.parse::<u64>().ok().map(std::time::Duration::from_secs)
}

pub(crate) fn sign(
    api_secret: &str,
    timestamp: &str,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    let Ok(mut mac) = HmacSha256::new_from_slice(api_secret.as_bytes()) else {
        unreachable!("HMAC-SHA256 accepts any key length")
    };
    mac.update(timestamp.as_bytes());
    mac.update(method.as_bytes());
    mac.update(path.as_bytes());
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests;
