use chrono::{DateTime, Utc};

/// Source of economic calendar events.
pub struct EconomicCalendar {
    _client: reqwest::Client,
}

/// Scheduled economic event.
pub struct EconomicEvent {
    /// Name of the event.
    pub title: String,
    /// Currency that the event affects.
    pub currency: String,
    /// Expected impact level of the event.
    pub impact: String,
    /// Time at which the event happens.
    pub datetime: DateTime<Utc>,
}

impl EconomicCalendar {
    /// Creates a calendar with its own HTTP client.
    ///
    /// # Errors
    ///
    /// - Returns an error if the HTTP client cannot be built.
    pub fn new() -> anyhow::Result<Self> {
        let _client = crate::http_client::build_http_client()?;
        Ok(Self { _client })
    }

    /// Returns the upcoming events; the result is always empty because the calendar is a stub.
    pub async fn fetch_upcoming(&self) -> Vec<EconomicEvent> {
        tracing::warn!("using stub implementation");
        Vec::new()
    }
}
