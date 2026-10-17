pub(crate) struct TaskHandles {
    pub(crate) feed_handles: Vec<tokio::task::AbortHandle>,
    pub(crate) macro_analyst: Option<tokio::task::JoinHandle<()>>,
    pub(crate) pipeline: super::pipeline::PipelineHandles,
    pub(crate) background: super::background::BackgroundHandles,
}

const DRAIN_TIMEOUT_SECS: u64 = 5;

fn abort_background_tasks(handles: &TaskHandles) {
    for h in &handles.feed_handles {
        h.abort();
    }
    handles.background.overnight.abort();
    handles.background.sfd.abort();
    handles.background.stop_fill.abort();
    handles.background.balance_drift.abort();
    if let Some(h) = &handles.background.feed_watchdog {
        h.abort();
    }
    handles.background.daily.abort();
    if let Some(h) = &handles.macro_analyst {
        h.abort();
    }
    handles.background.api.abort();
}

async fn drain_pipeline(handles: TaskHandles) {
    let drain_timeout = tokio::time::Duration::from_secs(DRAIN_TIMEOUT_SECS);
    let _ = tokio::time::timeout(drain_timeout, async {
        let _ = handles.pipeline.engine.await;
        let _ = handles.pipeline.pos_monitor.await;
        let _ = handles.pipeline.crypto_monitor.await;
        let _ = handles.pipeline.exit_executor.await;
        let _ = handles.pipeline.executor.await;
        let _ = handles.pipeline.recorder.await;
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
