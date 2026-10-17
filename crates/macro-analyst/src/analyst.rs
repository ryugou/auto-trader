use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::knowledge::{KnowledgeStore, MarketEvent};
use auto_trader_core::strategy::MacroUpdate;
use sqlx::PgPool;

use crate::calendar::EconomicCalendar;
use crate::news::NewsFetcher;
use crate::summarizer::GeminiSummarizer;

/// Analyst that periodically fetches news, summarizes each new item and publishes the summary as a macro update.
pub struct MacroAnalyst {
    _calendar: EconomicCalendar,
    news: NewsFetcher,
    summarizer: GeminiSummarizer,
    knowledge: Option<Arc<dyn KnowledgeStore>>,
    pool: Option<PgPool>,
    seen_titles: std::collections::HashSet<String>,
}

impl MacroAnalyst {
    /// Creates an analyst for the given news feed URLs and Gemini endpoint, with no knowledge store and no database.
    ///
    /// # Errors
    ///
    /// - Returns an error if the HTTP client of the calendar, the news fetcher or the summarizer cannot be built.
    pub fn new(
        news_sources: Vec<String>,
        gemini_api_url: &str,
        gemini_api_key: &str,
        gemini_model: &str,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            _calendar: EconomicCalendar::new()?,
            news: NewsFetcher::new(news_sources)?,
            summarizer: GeminiSummarizer::new(gemini_api_url, gemini_api_key, gemini_model)?,
            knowledge: None,
            pool: None,
            seen_titles: std::collections::HashSet::new(),
        })
    }

    /// Sets the knowledge store that receives a market event for each summary.
    pub fn with_knowledge(mut self, store: Arc<dyn KnowledgeStore>) -> Self {
        self.knowledge = Some(store);
        self
    }

    /// Sets the database pool that receives a macro event for each summary.
    pub fn with_db(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Fetches news every `news_interval` and sends a macro update on `macro_tx` for each new item, skipping titles already seen.
    ///
    /// The loop runs until the future is dropped. A failed summary is logged and the item is skipped; a failed store write or database write is logged and the macro update is still sent.
    ///
    /// # Errors
    ///
    /// - Never returns an error: the loop has no exit.
    ///
    /// # Panics
    ///
    /// - Panics if `news_interval` is zero.
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
                self.process_news_item(item, &macro_tx).await;
            }
        }
    }

    async fn process_news_item(
        &mut self,
        item: &crate::news::NewsItem,
        macro_tx: &tokio::sync::broadcast::Sender<MacroUpdate>,
    ) {
        if !self.seen_titles.insert(item.title.clone()) {
            return;
        }

        let combined = format!("{}: {}", item.title, item.description);
        let summary = match self.summarizer.summarize_for_fx(&combined).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to summarize news");
                return;
            }
        };

        self.persist_news_summary(&summary, &item.source).await;

        let update = MacroUpdate {
            summary: summary.clone(),
            adjustments: HashMap::new(),
        };
        let _ = macro_tx.send(update);
    }

    async fn persist_news_summary(&self, summary: &str, source: &str) {
        if let Some(store) = &self.knowledge {
            let event = MarketEvent {
                summary,
                event_type: "news",
                impact: "medium",
                timestamp: chrono::Utc::now(),
            };
            if let Err(e) = store.record_market_event(&event).await {
                tracing::warn!(error = %e, "failed to record market event");
            }
        }

        if let Some(pool) = &self.pool
            && let Err(e) = auto_trader_db::macro_events::insert_macro_event(
                pool,
                summary,
                "news",
                "medium",
                chrono::Utc::now(),
                Some(source),
            )
            .await
        {
            tracing::warn!(error = %e, "failed to insert macro event");
        }
    }
}
