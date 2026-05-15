use crate::enriched_ingest;
use async_trait::async_trait;
use auto_trader_core::knowledge::{
    KnowledgeStore, MarketEvent, PatternHit, PatternSearchResults, TradeCloseContext,
};
use auto_trader_core::types::{Pair, Trade};
use auto_trader_core::vegapunk_port::{SearchMode, VegapunkApi};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;

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
mod tests {
    use super::*;
    use auto_trader_core::types::{Direction, Exchange, Pair, Trade, TradeStatus};
    use auto_trader_core::vegapunk_port::{SearchMode, SearchResults, VegapunkApi};
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::sync::Arc;
    use uuid::Uuid;

    struct CaptureApi {
        captured: std::sync::Mutex<Vec<(String, String, String, String)>>,
    }

    #[async_trait::async_trait]
    impl VegapunkApi for CaptureApi {
        async fn ingest_raw(
            &self,
            text: &str,
            source_type: &str,
            channel: &str,
            timestamp: &str,
        ) -> anyhow::Result<()> {
            self.captured.lock().unwrap().push((
                text.into(),
                source_type.into(),
                channel.into(),
                timestamp.into(),
            ));
            Ok(())
        }
        async fn search(&self, _: &str, _: SearchMode, _: i32) -> anyhow::Result<SearchResults> {
            Ok(SearchResults {
                hits: vec![],
                search_id: "sid".into(),
            })
        }
        async fn feedback(&self, _: &str, _: i32, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn merge(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn fixture_trade() -> Trade {
        Trade {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            strategy_name: "test_strategy".to_string(),
            pair: Pair("USD_JPY".to_string()),
            exchange: Exchange::GmoFx,
            direction: Direction::Long,
            entry_price: Decimal::new(15000, 2),
            entry_at: Utc::now(),
            stop_loss: Decimal::new(14900, 2),
            take_profit: Some(Decimal::new(15200, 2)),
            quantity: Decimal::new(1000, 0),
            leverage: Decimal::ONE,
            status: TradeStatus::Open,
            pnl_amount: None,
            fees: Decimal::ZERO,
            exit_price: None,
            exit_at: None,
            exit_reason: None,
            max_hold_until: None,
            exchange_position_id: None,
        }
    }

    #[tokio::test]
    async fn record_trade_open_emits_ingest_with_correct_metadata() {
        let api = Arc::new(CaptureApi {
            captured: std::sync::Mutex::new(Vec::new()),
        });
        let store = VegapunkKnowledgeStore::new(api.clone() as Arc<dyn VegapunkApi>);

        let mut indicators = HashMap::new();
        indicators.insert("rsi".to_string(), Decimal::new(50, 0));
        let trade = fixture_trade();

        store
            .record_trade_open(&trade, &indicators, Some(Decimal::new(2, 2)))
            .await
            .unwrap();

        let captured = api.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let (text, source_type, channel, _ts) = &captured[0];
        assert_eq!(source_type, "trade_signal");
        assert_eq!(channel, "usd_jpy-trades");
        assert!(text.contains("USD_JPY"), "text should include pair");
        assert!(text.contains("ロング"), "text should include direction");
    }
}
