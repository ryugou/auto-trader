use auto_trader_core::event::{PriceEvent, SignalEvent, TradeEvent};
use auto_trader_core::strategy::MacroUpdate;
use tokio::sync::{broadcast, mpsc};

use super::pipeline::PipelineChannels;

pub(crate) struct Senders {
    pub(crate) price_tx: mpsc::Sender<PriceEvent>,
    pub(crate) signal_tx: mpsc::Sender<SignalEvent>,
    pub(crate) trade_tx: mpsc::Sender<TradeEvent>,
    pub(crate) macro_tx: broadcast::Sender<MacroUpdate>,
}

pub(crate) struct Channels {
    pub(crate) senders: Senders,
    pub(crate) pipeline: PipelineChannels,
}

pub(crate) fn open() -> Channels {
    let (price_tx, price_rx) = mpsc::channel::<PriceEvent>(256);
    let (price_monitor_tx, price_monitor_rx) = mpsc::channel::<PriceEvent>(256);
    let (signal_tx, signal_rx) = mpsc::channel::<SignalEvent>(256);
    let (trade_tx, trade_rx) = mpsc::channel::<TradeEvent>(256);
    let (macro_tx, _) = broadcast::channel::<MacroUpdate>(16);
    let macro_rx = macro_tx.subscribe();

    Channels {
        senders: Senders {
            price_tx,
            signal_tx,
            trade_tx,
            macro_tx,
        },
        pipeline: PipelineChannels {
            price_rx,
            price_monitor_tx,
            price_monitor_rx,
            signal_rx,
            trade_rx,
            macro_rx,
        },
    }
}
