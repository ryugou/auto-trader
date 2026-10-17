use async_trait::async_trait;

/// One hit returned by a Vegapunk search.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// Text of the hit.
    pub text: String,
    /// Relevance score of the hit.
    pub score: f32,
}

/// Result of a Vegapunk search.
#[derive(Debug, Clone)]
pub struct SearchResults {
    /// Hits of the search.
    pub hits: Vec<SearchHit>,
    /// Identifier of the search.
    pub search_id: String,
}

/// Mode of a Vegapunk search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Local search.
    Local,
    /// Global search.
    Global,
    /// Hybrid search.
    Hybrid,
}

impl SearchMode {
    /// Returns the lowercase name of the mode.
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchMode::Local => "local",
            SearchMode::Global => "global",
            SearchMode::Hybrid => "hybrid",
        }
    }
}

/// Client interface to the Vegapunk service.
#[async_trait]
pub trait VegapunkApi: Send + Sync {
    /// Ingests a raw text with its source type, channel and timestamp.
    async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<()>;

    /// Searches with `query` in the given mode, with `top_k` as the number of hits requested.
    async fn search(
        &self,
        query: &str,
        mode: SearchMode,
        top_k: i32,
    ) -> anyhow::Result<SearchResults>;

    /// Submits a rating and a comment for the search identified by `search_id`.
    async fn feedback(&self, search_id: &str, rating: i32, comment: &str) -> anyhow::Result<()>;

    /// Requests a merge of the ingested knowledge.
    async fn merge(&self) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests;
