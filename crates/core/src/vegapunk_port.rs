use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub text: String,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub search_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Local,
    Global,
    Hybrid,
}

impl SearchMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchMode::Local => "local",
            SearchMode::Global => "global",
            SearchMode::Hybrid => "hybrid",
        }
    }
}

#[async_trait]
pub trait VegapunkApi: Send + Sync {
    async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<()>;

    async fn search(
        &self,
        query: &str,
        mode: SearchMode,
        top_k: i32,
    ) -> anyhow::Result<SearchResults>;

    async fn feedback(&self, search_id: &str, rating: i32, comment: &str) -> anyhow::Result<()>;

    async fn merge(&self) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests;
