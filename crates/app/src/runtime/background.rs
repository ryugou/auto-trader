use auto_trader::api;
use auto_trader::balance_drift::BalanceDriftContext;
use auto_trader::feed_watchdog;
use auto_trader_core::config::AppConfig;
use auto_trader_core::event::TradeEvent;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::dependencies::SharedDependencies;
use super::{api_server, balance_drift_loop, daily_batch, overnight, sfd_accrual, stop_fill_loop};

pub(crate) struct BackgroundHandles {
    pub(crate) daily: JoinHandle<()>,
    pub(crate) stop_fill: JoinHandle<()>,
    pub(crate) balance_drift: JoinHandle<()>,
    pub(crate) feed_watchdog: Option<JoinHandle<()>>,
    pub(crate) overnight: JoinHandle<()>,
    pub(crate) sfd: JoinHandle<()>,
    pub(crate) api: JoinHandle<()>,
}

pub(crate) async fn spawn_all(
    dependencies: &SharedDependencies,
    trade_tx: &mpsc::Sender<TradeEvent>,
    config: &AppConfig,
    drift_ctx: BalanceDriftContext,
    live_forces_dry_run: bool,
) -> anyhow::Result<BackgroundHandles> {
    let daily = spawn_daily_batch(dependencies, config);

    let stop_fill = tokio::spawn(stop_fill_loop::run(
        dependencies.close_context(trade_tx),
        live_forces_dry_run,
    ));

    let balance_drift = tokio::spawn(balance_drift_loop::run(
        drift_ctx,
        dependencies.notifier.clone(),
    ));

    let feed_watchdog = spawn_feed_watchdog(dependencies, config);

    let overnight = tokio::spawn(overnight::run(
        dependencies.pool.clone(),
        config.gmo_fx.swap.clone(),
        dependencies.notifier.clone(),
    ));

    let sfd = tokio::spawn(sfd_accrual::run(
        dependencies.pool.clone(),
        dependencies.price_store.clone(),
    ));

    let api = api_server::spawn(api::AppState {
        pool: dependencies.pool.clone(),
        price_store: dependencies.price_store.clone(),
        exchange_liquidation_levels: dependencies.liquidation_levels.clone(),
    })
    .await?;

    Ok(BackgroundHandles {
        daily,
        stop_fill,
        balance_drift,
        feed_watchdog,
        overnight,
        sfd,
        api,
    })
}

fn spawn_daily_batch(dependencies: &SharedDependencies, config: &AppConfig) -> JoinHandle<()> {
    tokio::spawn(daily_batch::run(daily_batch::DailyBatchContext {
        pool: dependencies.pool.clone(),
        knowledge_store: dependencies.knowledge_store.clone(),
        gemini_api_url: config
            .gemini
            .as_ref()
            .map(|g| g.api_url.clone())
            .unwrap_or_default(),
        gemini_api_key: std::env::var("GEMINI_API_KEY").unwrap_or_default(),
        gemini_model: config
            .gemini
            .as_ref()
            .map(|g| g.model.clone())
            .unwrap_or_default(),
        backfill_days: config.monitor.backfill_days,
    }))
}

fn spawn_feed_watchdog(
    dependencies: &SharedDependencies,
    config: &AppConfig,
) -> Option<JoinHandle<()>> {
    if config.feed_watchdog.enabled {
        let feed_watchdog_ctx = feed_watchdog::FeedWatchdogContext {
            price_store: dependencies.price_store.clone(),
            notifier: dependencies.notifier.clone(),
            config: config.feed_watchdog.clone(),
        };
        Some(tokio::spawn(feed_watchdog::run(feed_watchdog_ctx)))
    } else {
        tracing::info!("feed watchdog disabled ([feed_watchdog].enabled=false)");
        None
    }
}
