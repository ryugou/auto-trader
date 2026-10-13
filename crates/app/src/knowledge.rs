use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use auto_trader_core::knowledge::{
    KnowledgeStore, MarketEvent, PatternHit, PatternSearchResults, TradeCloseContext,
};
use auto_trader_core::types::{Pair, Trade};
use auto_trader_core::vegapunk_port::{SearchMode, VegapunkApi};
use rust_decimal::Decimal;

use crate::enriched_ingest;

pub struct VegapunkKnowledgeStore {
    api: Arc<dyn VegapunkApi>,
}

impl VegapunkKnowledgeStore {
    pub fn new(api: Arc<dyn VegapunkApi>) -> Self {
        Self { api }
    }

    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn pair_channel(pair: &Pair) -> String {
        format!("{}-trades", pair.0.to_lowercase())
    }
}

#[async_trait]
impl KnowledgeStore for VegapunkKnowledgeStore {
    async fn record_trade_open(
        &self,
        trade: &Trade,
        indicators: &HashMap<String, Decimal>,
        allocation_pct: Option<Decimal>,
    ) -> anyhow::Result<()> {
        let text = enriched_ingest::format_trade_open(trade, indicators, allocation_pct);
        let channel = Self::pair_channel(&trade.pair);
        let timestamp = Self::now_rfc3339();
        self.api
            .ingest_raw(&text, "trade_signal", &channel, &timestamp)
            .await
    }

    async fn record_trade_close(
        &self,
        trade: &Trade,
        ctx: &TradeCloseContext<'_>,
    ) -> anyhow::Result<()> {
        let text = enriched_ingest::format_trade_close(
            trade,
            ctx.entry_indicators,
            ctx.account_balance,
            ctx.account_initial,
        );
        let channel = Self::pair_channel(&trade.pair);
        let timestamp = trade
            .exit_at
            .map(|e| e.to_rfc3339())
            .unwrap_or_else(Self::now_rfc3339);
        self.api
            .ingest_raw(&text, "trade_result", &channel, &timestamp)
            .await
    }

    async fn record_market_event(&self, event: &MarketEvent<'_>) -> anyhow::Result<()> {
        let text = format!(
            "[{}] {} (impact={})",
            event.event_type, event.summary, event.impact
        );
        let timestamp = event.timestamp.to_rfc3339();
        self.api
            .ingest_raw(&text, "market_event", "macro-events", &timestamp)
            .await
    }

    async fn search_similar_patterns(
        &self,
        pair: &Pair,
        current_price: Decimal,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults> {
        let query = format!(
            "{}の現在の市場状況とトレード判断。価格: {}",
            pair.0, current_price
        );
        let res = self.api.search(&query, SearchMode::Local, top_k).await?;
        Ok(PatternSearchResults {
            hits: res
                .hits
                .into_iter()
                .map(|h| PatternHit {
                    text: h.text,
                    score: h.score,
                })
                .collect(),
            search_id: res.search_id,
        })
    }

    async fn search_strategy_outcomes(
        &self,
        strategy_name: &str,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults> {
        let query = format!("{}戦略の勝率と傾向", strategy_name);
        let res = self.api.search(&query, SearchMode::Hybrid, top_k).await?;
        Ok(PatternSearchResults {
            hits: res
                .hits
                .into_iter()
                .map(|h| PatternHit {
                    text: h.text,
                    score: h.score,
                })
                .collect(),
            search_id: res.search_id,
        })
    }

    async fn submit_feedback(
        &self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()> {
        self.api.feedback(search_id, rating, comment).await
    }

    async fn run_merge(&self) -> anyhow::Result<()> {
        self.api.merge().await
    }
}

#[cfg(test)]
mod tests;
