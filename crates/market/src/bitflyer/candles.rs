use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::Exchange;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::mpsc;

use crate::indicators;
use crate::price_store::{FeedKey, LatestTick, PriceStore};

use super::PRICE_STORE_TICK_CHANNEL_CAP;

pub(super) fn log_seeded_warmup_closes(
    closes_map: &HashMap<String, Vec<Decimal>>,
    timeframe: &str,
) {
    for (pair, closes) in closes_map {
        tracing::info!(
            pair = %pair,
            timeframe = %timeframe,
            count = closes.len(),
            "seeded warmup closes"
        );
    }
}

pub(super) fn spawn_tick_drain(
    price_store: Arc<PriceStore>,
) -> mpsc::Sender<(FeedKey, LatestTick)> {
    let (tick_tx, mut tick_rx) =
        mpsc::channel::<(FeedKey, LatestTick)>(PRICE_STORE_TICK_CHANNEL_CAP);
    let ps = price_store.clone();
    tokio::spawn(async move {
        while let Some((key, tick)) = tick_rx.recv().await {
            ps.update(key, tick).await;
        }
    });
    tick_tx
}

fn emit_candle_event(
    candle: auto_trader_core::types::Candle,
    closes_map: &mut HashMap<String, Vec<Decimal>>,
    highs_map: &mut HashMap<String, Vec<Decimal>>,
    lows_map: &mut HashMap<String, Vec<Decimal>>,
    compute_full_indicators: bool,
) -> (PriceEvent, HashMap<String, Decimal>) {
    let product_code = candle.pair.0.clone();

    let closes = closes_map.entry(product_code.clone()).or_default();
    closes.push(candle.close);
    if closes.len() > 200 {
        closes.drain(..closes.len() - 200);
    }

    let highs = highs_map.entry(product_code.clone()).or_default();
    highs.push(candle.high);
    if highs.len() > 200 {
        highs.drain(..highs.len() - 200);
    }

    let lows = lows_map.entry(product_code.clone()).or_default();
    lows.push(candle.low);
    if lows.len() > 200 {
        lows.drain(..lows.len() - 200);
    }

    let indicator_map = if compute_full_indicators {
        indicators::indicator_map(closes, highs, lows)
    } else {
        HashMap::new()
    };

    let event = PriceEvent {
        pair: candle.pair.clone(),
        exchange: Exchange::BitflyerCfd,
        timestamp: candle.timestamp,
        candle,
        indicators: indicator_map.clone(),
    };
    (event, indicator_map)
}

pub(super) async fn process_primary_candle(
    candle: auto_trader_core::types::Candle,
    pool: Option<&PgPool>,
    closes_map: &mut HashMap<String, Vec<Decimal>>,
    highs_map: &mut HashMap<String, Vec<Decimal>>,
    lows_map: &mut HashMap<String, Vec<Decimal>>,
    price_tx: &mpsc::Sender<PriceEvent>,
    latest_indicators: &mut HashMap<String, HashMap<String, Decimal>>,
) -> bool {
    if let Some(pool) = pool
        && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &candle).await
    {
        tracing::warn!(error = %e, "failed to save crypto candle");
    }
    let pair_key = candle.pair.0.clone();
    let (event, indicators) = emit_candle_event(candle, closes_map, highs_map, lows_map, true);
    latest_indicators.insert(pair_key, indicators);
    if price_tx.send(event).await.is_err() {
        tracing::info!("price channel closed, stopping bitflyer monitor");
        return false;
    }
    true
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn process_h1_candle(
    h1_candle: auto_trader_core::types::Candle,
    pool: Option<&PgPool>,
    closes_map_h1: &mut HashMap<String, Vec<Decimal>>,
    highs_map_h1: &mut HashMap<String, Vec<Decimal>>,
    lows_map_h1: &mut HashMap<String, Vec<Decimal>>,
    price_tx: &mpsc::Sender<PriceEvent>,
    latest_indicators: &HashMap<String, HashMap<String, Decimal>>,
    product_code: &str,
    primary_tf_prefix: &str,
) -> bool {
    if let Some(pool) = pool
        && let Err(e) = auto_trader_db::candles::upsert_candle(pool, &h1_candle).await
    {
        tracing::warn!(error = %e, "failed to save h1 crypto candle");
    }
    let (mut h1_event, _h1_indicators) =
        emit_candle_event(h1_candle, closes_map_h1, highs_map_h1, lows_map_h1, true);
    if let Some(m5) = latest_indicators.get(product_code) {
        for (key, value) in m5 {
            h1_event
                .indicators
                .insert(format!("{primary_tf_prefix}_{key}"), *value);
        }
    }
    if price_tx.send(h1_event).await.is_err() {
        tracing::info!("price channel closed, stopping bitflyer monitor");
        return false;
    }
    true
}

#[cfg(test)]
mod tests;
