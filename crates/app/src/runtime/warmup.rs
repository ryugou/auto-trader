use std::collections::HashMap;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Candle, Exchange, Pair};
use auto_trader_strategy::engine::StrategyEngine;
use rust_decimal::Decimal;

pub(crate) struct WarmupTimeframes {
    pub(crate) crypto: &'static str,
    pub(crate) h1: &'static str,
    pub(crate) fx: &'static str,
    pub(crate) limit: i64,
}

#[derive(Default)]
pub(crate) struct BitflyerCandleSeed {
    pub(crate) highs: HashMap<String, Vec<Decimal>>,
    pub(crate) lows: HashMap<String, Vec<Decimal>>,
    pub(crate) closes: HashMap<String, Vec<Decimal>>,
}

async fn load_warmup_history(
    pool: &sqlx::PgPool,
    exchange: &str,
    pair: &str,
    timeframe: &str,
    limit: i64,
) -> Vec<Candle> {
    match auto_trader_db::candles::get_candles(pool, exchange, pair, timeframe, limit).await {
        Ok(most_recent_first) => most_recent_first.into_iter().rev().collect(),
        Err(e) => {
            tracing::warn!(
                exchange = %exchange,
                pair = %pair,
                timeframe = %timeframe,
                error = %e,
                "failed to load warmup history"
            );
            Vec::new()
        }
    }
}

fn to_price_events(candles: Vec<Candle>, exchange: Exchange) -> Vec<PriceEvent> {
    candles
        .into_iter()
        .map(|c| PriceEvent {
            pair: c.pair.clone(),
            exchange,
            timestamp: c.timestamp,
            candle: c,
            indicators: HashMap::new(),
        })
        .collect()
}

async fn warmup_pairs(
    pool: &sqlx::PgPool,
    engine: &mut StrategyEngine,
    exchange: Exchange,
    pairs: &[Pair],
    timeframe: &str,
    limit: i64,
    mut seed: Option<&mut BitflyerCandleSeed>,
) {
    for pair in pairs {
        let candles = load_warmup_history(pool, exchange.as_str(), &pair.0, timeframe, limit).await;
        if candles.is_empty() {
            continue;
        }
        if let Some(s) = seed.as_deref_mut() {
            s.highs
                .insert(pair.0.clone(), candles.iter().map(|c| c.high).collect());
            s.lows
                .insert(pair.0.clone(), candles.iter().map(|c| c.low).collect());
            s.closes
                .insert(pair.0.clone(), candles.iter().map(|c| c.close).collect());
        }
        let events = to_price_events(candles, exchange);
        engine.warmup(&events).await;
        tracing::info!(
            exchange = %exchange,
            timeframe = timeframe,
            pair = %pair.0,
            candle_count = events.len(),
            "fed warmup candles"
        );
    }
}

pub(crate) async fn feed_history(
    pool: &sqlx::PgPool,
    engine: &mut StrategyEngine,
    crypto_pairs: &[Pair],
    fx_pairs: &[Pair],
    timeframes: WarmupTimeframes,
) -> BitflyerCandleSeed {
    let mut seed = BitflyerCandleSeed::default();

    warmup_pairs(
        pool,
        engine,
        Exchange::BitflyerCfd,
        crypto_pairs,
        timeframes.crypto,
        timeframes.limit,
        Some(&mut seed),
    )
    .await;
    warmup_pairs(
        pool,
        engine,
        Exchange::BitflyerCfd,
        crypto_pairs,
        timeframes.h1,
        timeframes.limit,
        None,
    )
    .await;
    warmup_pairs(
        pool,
        engine,
        Exchange::Oanda,
        fx_pairs,
        timeframes.fx,
        timeframes.limit,
        None,
    )
    .await;
    warmup_pairs(
        pool,
        engine,
        Exchange::GmoFx,
        fx_pairs,
        timeframes.fx,
        timeframes.limit,
        None,
    )
    .await;
    warmup_pairs(
        pool,
        engine,
        Exchange::GmoFx,
        fx_pairs,
        timeframes.h1,
        timeframes.limit,
        None,
    )
    .await;

    seed
}

#[cfg(test)]
mod tests;
