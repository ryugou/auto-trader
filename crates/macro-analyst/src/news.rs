use chrono::{DateTime, Utc};

pub struct NewsFetcher {
    client: reqwest::Client,
    sources: Vec<String>,
}

pub struct NewsItem {
    pub title: String,
    pub description: String,
    pub published: Option<DateTime<Utc>>,
    pub source: String,
}

impl NewsFetcher {
    pub fn new(sources: Vec<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            sources,
        }
    }

    pub async fn fetch_latest(&self) -> Vec<NewsItem> {
        let mut items = Vec::new();
        for source in &self.sources {
            match self.fetch_feed(source).await {
                Ok(mut feed_items) => items.append(&mut feed_items),
                Err(e) => tracing::warn!("failed to fetch news from {source}: {e}"),
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
