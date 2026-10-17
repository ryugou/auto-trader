use auto_trader_core::event::PriceEvent;
use tokio::sync::mpsc;

pub(crate) fn spawn(mut rx: mpsc::Receiver<PriceEvent>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move { while rx.recv().await.is_some() {} })
}
