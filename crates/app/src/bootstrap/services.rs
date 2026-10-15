use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::config::AppConfig;
use auto_trader_core::types::Exchange;
use auto_trader_db::pool::create_pool;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_notify::Notifier;

pub(crate) struct Services {
    pub(crate) pool: sqlx::PgPool,
    pub(crate) exchange_apis: Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>,
    pub(crate) notifier: Arc<Notifier>,
}

pub(crate) async fn connect(config: &AppConfig) -> anyhow::Result<Services> {
    let pool = create_pool(&config.database.url).await?;
    tracing::info!("database connected");

    let exchange_apis = super::exchange_apis::build(config)?;

    let slack_webhook_url = std::env::var("SLACK_WEBHOOK_URL").ok();
    let notifier: Arc<Notifier> = Arc::new(Notifier::new(slack_webhook_url)?);

    Ok(Services {
        pool,
        exchange_apis,
        notifier,
    })
}
