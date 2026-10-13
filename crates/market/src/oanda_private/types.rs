use reqwest::StatusCode;

#[derive(Debug, thiserror::Error)]
pub enum OandaApiError {
    #[error("failed to send http request: {0}")]
    Http(reqwest::Error),
    #[error("failed to call oanda api: status={status} body={body}")]
    Api { status: StatusCode, body: String },
    #[error("failed to parse response body: {0}")]
    Parse(String),
    #[error("invalid config: {0}")]
    Config(String),
}

impl From<reqwest::Error> for OandaApiError {
    fn from(e: reqwest::Error) -> Self {
        OandaApiError::Http(e.without_url())
    }
}
