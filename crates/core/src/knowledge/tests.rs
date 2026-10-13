use super::*;

struct StubStore;

#[async_trait]
impl KnowledgeStore for StubStore {
    async fn record_trade_open(
        &self,
        _: &Trade,
        _: &HashMap<String, Decimal>,
        _: Option<Decimal>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn record_trade_close(&self, _: &Trade, _: &TradeCloseContext<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn record_market_event(&self, _: &MarketEvent<'_>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn search_similar_patterns(
        &self,
        _: &Pair,
        _: Decimal,
        _: i32,
    ) -> anyhow::Result<PatternSearchResults> {
        Ok(PatternSearchResults {
            hits: vec![],
            search_id: String::new(),
        })
    }

    async fn search_strategy_outcomes(
        &self,
        _: &str,
        _: i32,
    ) -> anyhow::Result<PatternSearchResults> {
        Ok(PatternSearchResults {
            hits: vec![],
            search_id: String::new(),
        })
    }

    async fn submit_feedback(&self, _: &str, _: i32, _: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn run_merge(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn trait_is_object_safe() {
    let store: std::sync::Arc<dyn KnowledgeStore> = std::sync::Arc::new(StubStore);
    let pair = Pair("USD_JPY".to_string());
    let res = store
        .search_similar_patterns(&pair, Decimal::new(15000, 2), 5)
        .await
        .unwrap();
    assert!(res.hits.is_empty());
}
