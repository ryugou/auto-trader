use crate::types::{Pair, Trade};
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PatternHit {
    pub text: String,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct PatternSearchResults {
    pub hits: Vec<PatternHit>,
    pub search_id: String,
}

#[derive(Debug, Clone)]
pub struct MarketEvent<'a> {
    pub summary: &'a str,
    pub event_type: &'a str,
    pub impact: &'a str,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// `record_trade_close` に必要なコンテキスト。caller が DB 等から取得して詰める。
/// `enriched_ingest::format_trade_close` の引数構成に追従。
#[derive(Debug, Clone, Copy)]
pub struct TradeCloseContext<'a> {
    pub entry_indicators: Option<&'a serde_json::Value>,
    pub account_balance: Option<Decimal>,
    pub account_initial: Option<Decimal>,
}

#[async_trait]
pub trait KnowledgeStore: Send + Sync {
    async fn record_trade_open(
        &self,
        trade: &Trade,
        indicators: &HashMap<String, Decimal>,
        allocation_pct: Option<Decimal>,
    ) -> anyhow::Result<()>;

    async fn record_trade_close(
        &self,
        trade: &Trade,
        ctx: &TradeCloseContext<'_>,
    ) -> anyhow::Result<()>;

    async fn record_market_event(&self, event: &MarketEvent<'_>) -> anyhow::Result<()>;

    async fn search_similar_patterns(
        &self,
        pair: &Pair,
        current_price: Decimal,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults>;

    async fn search_strategy_outcomes(
        &self,
        strategy_name: &str,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults>;

    async fn submit_feedback(
        &self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()>;

    async fn run_merge(&self) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Pair;
    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use std::collections::HashMap;

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

        async fn record_trade_close(
            &self,
            _: &Trade,
            _: &TradeCloseContext<'_>,
        ) -> anyhow::Result<()> {
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
}
