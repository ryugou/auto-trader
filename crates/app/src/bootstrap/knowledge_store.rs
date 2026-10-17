use std::sync::Arc;

use auto_trader::knowledge::VegapunkKnowledgeStore;
use auto_trader_core::config::VegapunkConfig;
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_vegapunk::client::VegapunkClient;

pub(crate) async fn connect(config: &VegapunkConfig) -> Option<Arc<dyn KnowledgeStore>> {
    let auth_token = std::env::var("VEGAPUNK_AUTH_TOKEN").ok();
    match VegapunkClient::connect(&config.endpoint, &config.schema, auth_token.as_deref()).await {
        Ok(client) => {
            tracing::info!(endpoint = %config.endpoint, "vegapunk connected");
            Some(Arc::new(VegapunkKnowledgeStore::new(Arc::new(client))) as Arc<dyn KnowledgeStore>)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to connect to vegapunk");
            None
        }
    }
}
