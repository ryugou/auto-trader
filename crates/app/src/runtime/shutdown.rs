pub(crate) struct TaskHandles {
    pub(crate) feed_handles: Vec<tokio::task::AbortHandle>,
    pub(crate) overnight: tokio::task::JoinHandle<()>,
    pub(crate) sfd: tokio::task::JoinHandle<()>,
    pub(crate) stop_fill: tokio::task::JoinHandle<()>,
    pub(crate) balance_drift: tokio::task::JoinHandle<()>,
    pub(crate) feed_watchdog: Option<tokio::task::JoinHandle<()>>,
    pub(crate) daily: tokio::task::JoinHandle<()>,
    pub(crate) macro_analyst: Option<tokio::task::JoinHandle<()>>,
    pub(crate) api: tokio::task::JoinHandle<()>,
    pub(crate) engine: tokio::task::JoinHandle<()>,
    pub(crate) pos_monitor: tokio::task::JoinHandle<()>,
    pub(crate) crypto_monitor: tokio::task::JoinHandle<()>,
    pub(crate) exit_executor: tokio::task::JoinHandle<()>,
    pub(crate) executor: tokio::task::JoinHandle<()>,
    pub(crate) recorder: tokio::task::JoinHandle<()>,
}

const DRAIN_TIMEOUT_SECS: u64 = 5;

fn abort_background_tasks(handles: &TaskHandles) {
    for h in &handles.feed_handles {
        h.abort();
    }
    handles.overnight.abort();
    handles.sfd.abort();
    handles.stop_fill.abort();
    handles.balance_drift.abort();
    if let Some(h) = &handles.feed_watchdog {
        h.abort();
    }
    handles.daily.abort();
    if let Some(h) = &handles.macro_analyst {
        h.abort();
    }
    handles.api.abort();
}

async fn drain_pipeline(handles: TaskHandles) {
    let drain_timeout = tokio::time::Duration::from_secs(DRAIN_TIMEOUT_SECS);
    let _ = tokio::time::timeout(drain_timeout, async {
        let _ = handles.engine.await;
        let _ = handles.pos_monitor.await;
        let _ = handles.crypto_monitor.await;
        let _ = handles.exit_executor.await;
        let _ = handles.executor.await;
        let _ = handles.recorder.await;
    })
    .await;
}

pub(crate) async fn run(
    handles: TaskHandles,
    price_tx: tokio::sync::mpsc::Sender<auto_trader_core::event::PriceEvent>,
    trade_tx: tokio::sync::mpsc::Sender<auto_trader_core::event::TradeEvent>,
) {
    tracing::info!("shutting down... draining channels");

    drop(price_tx);
    drop(trade_tx);

    abort_background_tasks(&handles);
    drain_pipeline(handles).await;

    tracing::info!("shutdown complete");
}
