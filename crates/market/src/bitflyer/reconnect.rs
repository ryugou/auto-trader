use auto_trader_core::event::PriceEvent;
use tokio::sync::mpsc;

use crate::price_store::{FeedKey, LatestTick};

fn handle_connect_success(
    price_tx: &mpsc::Sender<PriceEvent>,
    backoff_secs: &mut u64,
) -> Option<anyhow::Result<()>> {
    if price_tx.is_closed() {
        tracing::info!("price channel closed, stopping bitflyer monitor");
        return Some(Ok(()));
    }
    tracing::info!("bitflyer websocket closed normally, reconnecting");
    *backoff_secs = 1;
    None
}

async fn handle_connect_failure(
    e: anyhow::Error,
    price_tx: &mpsc::Sender<PriceEvent>,
    tick_tx: &mpsc::Sender<(FeedKey, LatestTick)>,
    backoff_secs: &mut u64,
) -> Option<anyhow::Result<()>> {
    if price_tx.is_closed() {
        tracing::info!("price channel closed, stopping bitflyer monitor");
        return Some(Ok(()));
    }
    if tick_tx.is_closed() {
        tracing::error!(
            "failed to forward ticks: drain channel closed, stopping feed without reconnect"
        );
        return Some(Err(e));
    }
    tracing::warn!(
        backoff_secs = *backoff_secs,
        error = %e,
        "failed to stream websocket messages, reconnecting"
    );
    tokio::time::sleep(std::time::Duration::from_secs(*backoff_secs)).await;
    *backoff_secs = (*backoff_secs * 2).min(60);
    None
}

pub(super) async fn handle_stream_result(
    result: anyhow::Result<()>,
    price_tx: &mpsc::Sender<PriceEvent>,
    tick_tx: &mpsc::Sender<(FeedKey, LatestTick)>,
    backoff_secs: &mut u64,
) -> Option<anyhow::Result<()>> {
    match result {
        Ok(()) => handle_connect_success(price_tx, backoff_secs),
        Err(e) => handle_connect_failure(e, price_tx, tick_tx, backoff_secs).await,
    }
}
