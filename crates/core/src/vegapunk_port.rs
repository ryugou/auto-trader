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
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct StubApi;

    #[async_trait]
    impl VegapunkApi for StubApi {
        async fn ingest_raw(&self, _: &str, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn search(&self, _: &str, _: SearchMode, _: i32) -> anyhow::Result<SearchResults> {
            Ok(SearchResults {
                hits: vec![],
                search_id: String::new(),
            })
        }
        async fn feedback(&self, _: &str, _: i32, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn merge(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn trait_is_object_safe_and_callable() {
        let api: std::sync::Arc<dyn VegapunkApi> = std::sync::Arc::new(StubApi);
        api.ingest_raw("t", "s", "c", "ts").await.unwrap();
        let r = api.search("q", SearchMode::Local, 5).await.unwrap();
        assert_eq!(r.hits.len(), 0);
        api.feedback("sid", 5, "c").await.unwrap();
        api.merge().await.unwrap();
        assert_eq!(SearchMode::Local.as_str(), "local");
        assert_eq!(SearchMode::Global.as_str(), "global");
        assert_eq!(SearchMode::Hybrid.as_str(), "hybrid");
    }
}
