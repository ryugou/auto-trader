use chrono::{DateTime, Utc};

pub struct EconomicCalendar {
    _client: reqwest::Client,
}

pub struct EconomicEvent {
    pub title: String,
    pub currency: String,
    pub impact: String,
    pub datetime: DateTime<Utc>,
}

impl EconomicCalendar {
    pub fn new() -> anyhow::Result<Self> {
        let _client = crate::http_client::build_http_client()?;
        Ok(Self { _client })
    }

    pub async fn fetch_upcoming(&self) -> Vec<EconomicEvent> {
        tracing::warn!("using stub implementation");
        Vec::new()
    }
}
