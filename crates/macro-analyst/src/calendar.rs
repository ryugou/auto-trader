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
    pub fn new() -> Self {
        Self {
            _client: reqwest::Client::new(),
        }
    }

    pub async fn fetch_upcoming(&self) -> Vec<EconomicEvent> {
        // Phase 0 stub: concrete source to be selected at runtime
        tracing::warn!("economic calendar: using stub implementation");
        Vec::new()
    }
}

impl Default for EconomicCalendar {
    fn default() -> Self {
        Self::new()
    }
}
