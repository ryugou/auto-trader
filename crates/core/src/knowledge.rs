use std::collections::HashMap;

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::types::{Pair, Trade};

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
mod tests;
