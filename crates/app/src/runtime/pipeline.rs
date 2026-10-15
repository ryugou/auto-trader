use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use auto_trader_core::event::{PriceEvent, SignalEvent, TradeEvent};
use auto_trader_core::strategy::{ExitSignal, MacroUpdate};
use auto_trader_core::types::Exchange;
use auto_trader_strategy::engine::StrategyEngine;
use rust_decimal::Decimal;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::dependencies::SharedDependencies;
use super::engine_loop::{self, EngineLoopContext};
use super::executor_loop::{self, ExecutorLoopContext};
use super::{crypto_monitor, exit_executor, fx_price_drain, recorder};
use crate::bootstrap::live_flags::LiveFlags;

pub(crate) struct RiskLimits {
    pub(crate) price_freshness_secs: u64,
    pub(crate) daily_loss_limit_pct: Decimal,
    pub(crate) halt_hours: u64,
}

pub(crate) struct PipelineChannels {
    pub(crate) price_rx: mpsc::Receiver<PriceEvent>,
    pub(crate) price_monitor_tx: mpsc::Sender<PriceEvent>,
    pub(crate) price_monitor_rx: mpsc::Receiver<PriceEvent>,
    pub(crate) signal_rx: mpsc::Receiver<SignalEvent>,
    pub(crate) trade_rx: mpsc::Receiver<TradeEvent>,
    pub(crate) macro_rx: broadcast::Receiver<MacroUpdate>,
}

pub(crate) struct PipelineInputs {
    pub(crate) engine: StrategyEngine,
    pub(crate) exchange_pairs: Arc<HashMap<Exchange, HashSet<String>>>,
    pub(crate) live: LiveFlags,
    pub(crate) limits: RiskLimits,
    pub(crate) channels: PipelineChannels,
}

pub(crate) struct PipelineHandles {
    pub(crate) pos_monitor: JoinHandle<()>,
    pub(crate) crypto_monitor: JoinHandle<()>,
    pub(crate) engine: JoinHandle<()>,
    pub(crate) exit_executor: JoinHandle<()>,
    pub(crate) executor: JoinHandle<()>,
    pub(crate) recorder: JoinHandle<()>,
}

pub(crate) fn spawn_all(
    dependencies: &SharedDependencies,
    trade_tx: &mpsc::Sender<TradeEvent>,
    inputs: PipelineInputs,
) -> PipelineHandles {
    let pos_monitor = fx_price_drain::spawn(inputs.channels.price_monitor_rx);

    let (crypto_price_tx, crypto_price_rx) = mpsc::channel::<PriceEvent>(256);
    let crypto_monitor = tokio::spawn(crypto_monitor::run(
        dependencies.close_context(trade_tx),
        inputs.live.forces_dry_run,
        crypto_price_rx,
    ));

    let (exit_tx, exit_rx) = mpsc::channel::<ExitSignal>(64);
    let engine = tokio::spawn(engine_loop::run(
        EngineLoopContext {
            engine: inputs.engine,
            pool: dependencies.pool.clone(),
            price_store: dependencies.price_store.clone(),
            price_monitor_tx: inputs.channels.price_monitor_tx,
            crypto_price_tx,
            exit_tx,
        },
        inputs.channels.price_rx,
        inputs.channels.macro_rx,
    ));

    let exit_executor = tokio::spawn(exit_executor::run(
        dependencies.close_context(trade_tx),
        inputs.live.forces_dry_run,
        exit_rx,
    ));

    let executor = spawn_executor(
        dependencies,
        trade_tx,
        inputs.exchange_pairs,
        inputs.live,
        inputs.limits,
        inputs.channels.signal_rx,
    );

    let recorder = tokio::spawn(recorder::run(
        dependencies.pool.clone(),
        dependencies.knowledge_store.clone(),
        inputs.channels.trade_rx,
    ));

    PipelineHandles {
        pos_monitor,
        crypto_monitor,
        engine,
        exit_executor,
        executor,
        recorder,
    }
}

fn spawn_executor(
    dependencies: &SharedDependencies,
    trade_tx: &mpsc::Sender<TradeEvent>,
    exchange_pairs: Arc<HashMap<Exchange, HashSet<String>>>,
    live: LiveFlags,
    limits: RiskLimits,
    signal_rx: mpsc::Receiver<SignalEvent>,
) -> JoinHandle<()> {
    let ctx = ExecutorLoopContext {
        pool: dependencies.pool.clone(),
        trade_tx: trade_tx.clone(),
        exchange_apis: dependencies.exchange_apis.clone(),
        price_store: dependencies.price_store.clone(),
        notifier: dependencies.notifier.clone(),
        exchange_pairs,
        position_sizer: dependencies.position_sizer.clone(),
        exchange_liquidation_levels: dependencies.liquidation_levels.clone(),
        live_forces_dry_run: live.forces_dry_run,
        live_enabled: live.enabled,
        price_freshness_secs: limits.price_freshness_secs,
        daily_loss_limit_pct: limits.daily_loss_limit_pct,
        halt_hours: limits.halt_hours,
        knowledge_store: dependencies.knowledge_store.clone(),
    };
    tokio::spawn(executor_loop::run(ctx, signal_rx))
}
