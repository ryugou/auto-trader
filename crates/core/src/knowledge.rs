use std::collections::HashMap;

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::types::{Pair, Trade};

/// One pattern returned by a knowledge store search.
#[derive(Debug, Clone)]
pub struct PatternHit {
    /// Text of the pattern.
    pub text: String,
    /// Relevance score of the hit.
    pub score: f32,
}

/// Result of a knowledge store pattern search.
#[derive(Debug, Clone)]
pub struct PatternSearchResults {
    /// Hits of the search.
    pub hits: Vec<PatternHit>,
    /// Identifier of the search.
    pub search_id: String,
}

/// Market event to record in a knowledge store.
#[derive(Debug, Clone)]
pub struct MarketEvent<'a> {
    /// Summary of the event.
    pub summary: &'a str,
    /// Type of the event.
    pub event_type: &'a str,
    /// Impact of the event.
    pub impact: &'a str,
    /// Time of the event.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Context recorded together with a closed trade.
#[derive(Debug, Clone, Copy)]
pub struct TradeCloseContext<'a> {
    /// Indicator values at entry as JSON, if known.
    pub entry_indicators: Option<&'a serde_json::Value>,
    /// Balance of the account, if known.
    pub account_balance: Option<Decimal>,
    /// Initial balance of the account, if known.
    pub account_initial: Option<Decimal>,
}

/// Store that records trades and market events and searches what it recorded.
#[async_trait]
pub trait KnowledgeStore: Send + Sync {
    /// Records an opened trade with its indicator values and allocation percentage.
    async fn record_trade_open(
        &self,
        trade: &Trade,
        indicators: &HashMap<String, Decimal>,
        allocation_pct: Option<Decimal>,
    ) -> anyhow::Result<()>;

    /// Records a closed trade with its close context.
    async fn record_trade_close(
        &self,
        trade: &Trade,
        ctx: &TradeCloseContext<'_>,
    ) -> anyhow::Result<()>;

    /// Records a market event.
    async fn record_market_event(&self, event: &MarketEvent<'_>) -> anyhow::Result<()>;

    /// Searches for patterns similar to the current price of the pair, with `top_k` as the number of hits requested.
    async fn search_similar_patterns(
        &self,
        pair: &Pair,
        current_price: Decimal,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults>;

    /// Searches for the outcomes of the strategy, with `top_k` as the number of hits requested.
    async fn search_strategy_outcomes(
        &self,
        strategy_name: &str,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults>;

    /// Submits a rating and a comment for the search identified by `search_id`.
    async fn submit_feedback(
        &self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()>;

    /// Requests a merge of the recorded knowledge.
    async fn run_merge(&self) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests;
