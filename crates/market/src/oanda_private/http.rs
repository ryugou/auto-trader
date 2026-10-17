use anyhow::Context as _;
use reqwest::{Client, Method};

use crate::bitflyer_private::{MAX_ERROR_BODY_CHARS, truncate_body};

use super::types::OandaApiError;

pub(super) fn encode_path(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

/// Client of the OANDA private REST API for one account.
pub struct OandaPrivateApi {
    client: Client,
    base_url: String,
    account_id: String,
    api_key: String,
}

impl OandaPrivateApi {
    /// Creates a client of the API at `base_url` for the account `account_id` that authorizes its requests with `api_key`, with a request timeout of 30 seconds.
    ///
    /// # Errors
    ///
    /// - Returns an error if the HTTP client cannot be built.
    pub fn new(base_url: String, account_id: String, api_key: String) -> anyhow::Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .context("failed to build reqwest client")?,
            base_url,
            account_id,
            api_key,
        })
    }

    pub(super) fn account_id(&self) -> &str {
        &self.account_id
    }

    pub(super) fn authed(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Accept-Datetime-Format", "RFC3339")
            .header("Content-Type", "application/json")
    }

    pub(super) fn sanitize_body_snippet(&self, body: &str) -> String {
        let redacted = if body.contains(&self.account_id) {
            body.replace(&self.account_id, "<REDACTED_ACCOUNT_ID>")
        } else {
            body.to_string()
        };
        let truncated = truncate_body(&redacted);
        if redacted.chars().count() > MAX_ERROR_BODY_CHARS {
            format!(
                "{}... (truncated from {} chars)",
                truncated,
                redacted.chars().count()
            )
        } else {
            truncated
        }
    }

    pub(super) async fn send_json(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> anyhow::Result<serde_json::Value> {
        let resp = builder.send().await.map_err(OandaApiError::from)?;
        let status = resp.status();
        let text = resp.text().await.map_err(OandaApiError::from)?;
        if !status.is_success() {
            let final_body = self.sanitize_body_snippet(&text);
            return Err(anyhow::Error::from(OandaApiError::Api {
                status,
                body: final_body,
            }));
        }
        serde_json::from_str(&text)
            .map_err(|e| anyhow::Error::from(OandaApiError::Parse(e.to_string())))
    }
}

#[cfg(test)]
mod tests;
