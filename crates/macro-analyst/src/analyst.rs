use crate::calendar::EconomicCalendar;
use crate::news::NewsFetcher;
use crate::summarizer::GeminiSummarizer;
use auto_trader_core::strategy::MacroUpdate;
use auto_trader_vegapunk::client::VegapunkClient;
use sqlx::PgPool;
use std::collections::HashMap;

pub struct MacroAnalyst {
    _calendar: EconomicCalendar,
    news: NewsFetcher,
    summarizer: GeminiSummarizer,
    vegapunk: Option<VegapunkClient>,
    pool: Option<PgPool>,
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
            vegapunk: None,
            pool: None,
        }
    }

    pub fn with_vegapunk(mut self, client: VegapunkClient) -> Self {
        self.vegapunk = Some(client);
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
                let combined = format!("{}: {}", item.title, item.description);
                let summary = match self.summarizer.summarize_for_fx(&combined).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("summarization failed: {e}");
                        continue;
                    }
                };

                if let Some(vp) = &mut self.vegapunk {
                    let timestamp = chrono::Utc::now().to_rfc3339();
                    if let Err(e) = vp
                        .ingest_raw(&summary, "market_event", "macro-events", &timestamp)
                        .await
                    {
                        tracing::warn!("vegapunk ingest failed: {e}");
                    }
                }

                if let Some(pool) = &self.pool {
                    if let Err(e) = auto_trader_db::macro_events::insert_macro_event(
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
