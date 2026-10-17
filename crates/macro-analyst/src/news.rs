use chrono::{DateTime, Utc};

/// Fetcher of news items from RSS and Atom feeds.
pub struct NewsFetcher {
    client: reqwest::Client,
    sources: Vec<String>,
}

/// News item taken from a feed entry.
pub struct NewsItem {
    /// Title of the entry, empty when the entry has none.
    pub title: String,
    /// Summary of the entry, empty when the entry has none.
    pub description: String,
    /// Publication time of the entry, if the feed gives one.
    pub published: Option<DateTime<Utc>>,
    /// URL of the feed that the entry came from.
    pub source: String,
}

impl NewsFetcher {
    /// Creates a fetcher for the given feed URLs.
    ///
    /// # Errors
    ///
    /// - Returns an error if the HTTP client cannot be built.
    pub fn new(sources: Vec<String>) -> anyhow::Result<Self> {
        let client = crate::http_client::build_http_client()?;
        Ok(Self { client, sources })
    }

    /// Fetches the first 10 entries of every feed; a feed that fails to download or parse is logged and skipped.
    pub async fn fetch_latest(&self) -> Vec<NewsItem> {
        let mut items = Vec::new();
        for source in &self.sources {
            match self.fetch_feed(source).await {
                Ok(mut feed_items) => items.append(&mut feed_items),
                Err(e) => tracing::warn!(source = %source, error = %e, "failed to fetch news"),
            }
        }
        items
    }

    async fn fetch_feed(&self, url: &str) -> anyhow::Result<Vec<NewsItem>> {
        let body = self.client.get(url).send().await?.text().await?;
        let feed = feed_rs::parser::parse(body.as_bytes())?;
        let items = feed
            .entries
            .into_iter()
            .take(10)
            .map(|entry| NewsItem {
                title: entry.title.map(|t| t.content).unwrap_or_default(),
                description: entry.summary.map(|s| s.content).unwrap_or_default(),
                published: entry.published.map(|d| d.with_timezone(&Utc)),
                source: url.to_string(),
            })
            .collect();
        Ok(items)
    }
}
