use crate::calendar::EconomicCalendar;
use crate::news::NewsFetcher;
use crate::summarizer::GeminiSummarizer;
use auto_trader_core::knowledge::{KnowledgeStore, MarketEvent};
use auto_trader_core::strategy::MacroUpdate;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;

pub struct MacroAnalyst {
    _calendar: EconomicCalendar,
    news: NewsFetcher,
    summarizer: GeminiSummarizer,
    knowledge: Option<Arc<dyn KnowledgeStore>>,
    pool: Option<PgPool>,
    seen_titles: std::collections::HashSet<String>,
}

impl MacroAnalyst {
    pub fn new(
        news_sources: Vec<String>,
        gemini_api_url: &str,
        gemini_api_key: &str,
        gemini_model: &str,
    ) -> Self {
        Self {
            _calendar: EconomicCalendar::new(),
            news: NewsFetcher::new(news_sources),
            summarizer: GeminiSummarizer::new(gemini_api_url, gemini_api_key, gemini_model),
            knowledge: None,
            pool: None,
            seen_titles: std::collections::HashSet::new(),
        }
    }

    pub fn with_knowledge(mut self, store: Arc<dyn KnowledgeStore>) -> Self {
        self.knowledge = Some(store);
        self
    }

    pub fn with_db(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub async fn run(
        &mut self,
        macro_tx: tokio::sync::broadcast::Sender<MacroUpdate>,
        news_interval: std::time::Duration,
    ) -> anyhow::Result<()> {
        let mut tick = tokio::time::interval(news_interval);
        loop {
            tick.tick().await;

            let news_items = self.news.fetch_latest().await;
            for item in &news_items {
                // Skip already-processed items (dedup by title)
                if !self.seen_titles.insert(item.title.clone()) {
                    continue;
                }
                let combined = format!("{}: {}", item.title, item.description);
                let summary = match self.summarizer.summarize_for_fx(&combined).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("summarization failed: {e}");
                        continue;
                    }
                };

                if let Some(store) = &self.knowledge {
                    let event = MarketEvent {
                        summary: &summary,
                        event_type: "news",
                        impact: "medium",
                        timestamp: chrono::Utc::now(),
                    };
                    if let Err(e) = store.record_market_event(&event).await {
                        tracing::warn!("knowledge_store record_market_event failed: {e}");
                    }
                }

                if let Some(pool) = &self.pool
                    && let Err(e) = auto_trader_db::macro_events::insert_macro_event(
                        pool,
                        &summary,
                        "news",
                        "medium",
                        chrono::Utc::now(),
                        Some(&item.source),
                    )
                    .await
                {
                    tracing::warn!("failed to insert macro event: {e}");
                }

                let update = MacroUpdate {
                    summary: summary.clone(),
                    adjustments: HashMap::new(),
                };
                let _ = macro_tx.send(update);
            }
        }
    }
}
