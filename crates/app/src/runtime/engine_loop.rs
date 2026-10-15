use std::collections::HashMap;
use std::sync::Arc;

use auto_trader::price_store;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{ExitSignal, MacroUpdate};
use auto_trader_core::types::{Exchange, Position};
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_strategy::engine::StrategyEngine;
use tokio::sync::{broadcast, mpsc};

pub(crate) struct EngineLoopContext {
    pub(crate) engine: StrategyEngine,
    pub(crate) pool: sqlx::PgPool,
    pub(crate) price_store: Arc<price_store::PriceStore>,
    pub(crate) price_monitor_tx: mpsc::Sender<PriceEvent>,
    pub(crate) crypto_price_tx: mpsc::Sender<PriceEvent>,
    pub(crate) exit_tx: mpsc::Sender<ExitSignal>,
}

pub(crate) async fn run(
    mut ctx: EngineLoopContext,
    mut price_rx: mpsc::Receiver<PriceEvent>,
    mut macro_rx: broadcast::Receiver<MacroUpdate>,
) {
    loop {
        tokio::select! {
            price = price_rx.recv() => {
                match price {
                    Some(event) => handle_price_event(&mut ctx, event).await,
                    None => break,
                }
            }
            macro_update = macro_rx.recv() => {
                handle_macro_update(&mut ctx.engine, macro_update);
            }
        }
    }
}

async fn handle_price_event(ctx: &mut EngineLoopContext, event: PriceEvent) {
    update_price_store_from_candle_path_the_only_source_for_exchanges_without_raw_tick_feed(
        &ctx.price_store,
        price_store::FeedKey::new(event.exchange, event.pair.clone()),
        price_store::LatestTick {
            price: event.candle.close,
            best_bid: event.candle.best_bid,
            best_ask: event.candle.best_ask,
            ts: event.timestamp,
        },
    )
    .await;
    forward_to_position_monitor(&ctx.price_monitor_tx, &ctx.crypto_price_tx, &event).await;

    let positions_by_strategy = load_positions_by_strategy(&ctx.pool, &event).await;
    let exits = ctx
        .engine
        .on_price_with_positions(&event, &positions_by_strategy)
        .await;
    send_exits(&ctx.exit_tx, exits).await;
}

async fn update_price_store_from_candle_path_the_only_source_for_exchanges_without_raw_tick_feed(
    price_store: &price_store::PriceStore,
    key: price_store::FeedKey,
    tick: price_store::LatestTick,
) {
    price_store.update(key, tick).await;
}

async fn forward_to_position_monitor(
    price_monitor_tx: &mpsc::Sender<PriceEvent>,
    crypto_price_tx: &mpsc::Sender<PriceEvent>,
    event: &PriceEvent,
) {
    match event.exchange {
        Exchange::Oanda => {
            if price_monitor_tx.send(event.clone()).await.is_err() {
                tracing::warn!("fx position monitor channel closed");
            }
        }
        Exchange::BitflyerCfd | Exchange::GmoFx => {
            if crypto_price_tx.send(event.clone()).await.is_err() {
                tracing::warn!("position monitor channel closed");
            }
        }
    }
}

async fn load_positions_by_strategy(
    pool: &sqlx::PgPool,
    event: &PriceEvent,
) -> HashMap<String, Vec<Position>> {
    match auto_trader_db::trades::list_open_with_account_name_for_pair(
        pool,
        event.exchange.as_str(),
        &event.pair.0,
    )
    .await
    {
        Ok(rows) => group_positions_by_strategy(rows),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to list open trades for on_open_positions"
            );
            HashMap::new()
        }
    }
}

fn group_positions_by_strategy(rows: Vec<OpenTradeWithAccount>) -> HashMap<String, Vec<Position>> {
    let mut by_strategy: HashMap<String, Vec<Position>> = HashMap::new();
    for r in rows {
        by_strategy
            .entry(r.trade.strategy_name.clone())
            .or_default()
            .push(Position { trade: r.trade });
    }
    by_strategy
}

async fn send_exits(exit_tx: &mpsc::Sender<ExitSignal>, exits: Vec<ExitSignal>) {
    for exit in exits {
        if exit_tx.send(exit).await.is_err() {
            tracing::warn!("failed to send strategy exit: channel closed");
        }
    }
}

fn handle_macro_update(
    engine: &mut StrategyEngine,
    macro_update: Result<MacroUpdate, broadcast::error::RecvError>,
) {
    match macro_update {
        Ok(update) => engine.on_macro_update(&update),
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped_updates = n,
                "failed to receive macro broadcast: lagged"
            );
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::info!("macro broadcast channel closed");
        }
    }
}

#[cfg(test)]
mod tests;
