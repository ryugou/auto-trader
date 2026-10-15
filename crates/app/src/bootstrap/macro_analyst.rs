use std::sync::Arc;
use std::time::Duration;

use auto_trader_core::config::AppConfig;
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_core::strategy::MacroUpdate;
use auto_trader_macro_analyst::analyst::MacroAnalyst;
use tokio::sync::broadcast;

pub(crate) fn spawn(
    config: &AppConfig,
    pool: &sqlx::PgPool,
    knowledge_store: &Option<Arc<dyn KnowledgeStore>>,
    macro_tx: broadcast::Sender<MacroUpdate>,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let Some(mac) = config.macro_analyst.as_ref().filter(|m| m.enabled) else {
        tracing::info!("macro analyst disabled");
        return Ok(None);
    };

    match (resolve_gemini_api_key(), config.gemini.as_ref()) {
        (Some(_), None) | (None, _) => {
            tracing::info!(
                "missing env var \"GEMINI_API_KEY\" or gemini config, skipping macro analyst"
            );
            Ok(None)
        }
        (Some(api_key), Some(gemini_config)) => {
            let mut analyst = MacroAnalyst::new(
                mac.news_sources.clone(),
                &gemini_config.api_url,
                &api_key,
                &gemini_config.model,
            )?
            .with_db(pool.clone());

            if let Some(store) = knowledge_store {
                analyst = analyst.with_knowledge(Arc::clone(store));
            }

            let news_interval = Duration::from_secs(mac.news_interval_secs);
            Ok(Some(tokio::spawn(async move {
                if let Err(e) = analyst.run(macro_tx, news_interval).await {
                    tracing::error!(error = %e, "failed to run macro analyst");
                }
            })))
        }
    }
}

fn resolve_gemini_api_key() -> Option<String> {
    match std::env::var("GEMINI_API_KEY") {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => {
            tracing::warn!("invalid env var \"GEMINI_API_KEY\": not set or empty");
            None
        }
    }
}
