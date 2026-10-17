use reqwest::StatusCode;

/// Error of a call to the OANDA private API.
#[derive(Debug, thiserror::Error)]
pub enum OandaApiError {
    /// The HTTP request failed.
    #[error("failed to send http request: {0}")]
    Http(reqwest::Error),
    /// The API answered with an error status.
    #[error("failed to call oanda api: status={status} body={body}")]
    Api {
        /// HTTP status of the response.
        status: StatusCode,
        /// Body of the response.
        body: String,
    },
    /// The response body cannot be parsed, with the cause.
    #[error("failed to parse response body: {0}")]
    Parse(String),
    /// The configuration is invalid, with the cause.
    #[error("invalid config: {0}")]
    Config(String),
}

impl From<reqwest::Error> for OandaApiError {
    fn from(e: reqwest::Error) -> Self {
        OandaApiError::Http(e.without_url())
    }
}
