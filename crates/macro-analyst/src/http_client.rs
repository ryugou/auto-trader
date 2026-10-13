use std::time::Duration;

use anyhow::Context;

const HTTP_TIMEOUT_SECS: u64 = 30;

pub(crate) fn build_http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .context("failed to build http client")
}
