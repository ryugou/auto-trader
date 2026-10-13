use std::collections::HashMap;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::candle_builder::CandleBuilder;
use crate::price_store::{FeedKey, LatestTick};

use super::candles::{process_h1_candle, process_primary_candle};
use super::types::{JsonRpcMessage, TickerMessage};

async fn subscribe_to_pairs<S>(write: &mut S, pairs: &[Pair]) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    for pair in pairs {
        let subscribe = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "subscribe",
            "params": { "channel": format!("lightning_ticker_{}", pair.0) }
        });
        write.send(Message::Text(subscribe.to_string())).await?;
    }
    Ok(())
}

async fn receive_next_message<S>(
    read: &mut S,
    heartbeat_timeout: std::time::Duration,
) -> anyhow::Result<Option<Message>>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match tokio::time::timeout(heartbeat_timeout, read.next()).await {
        Ok(Some(msg)) => Ok(Some(msg?)),
        Ok(None) => Ok(None),
        Err(_) => {
            anyhow::bail!(
                "failed to receive websocket message: no message for {}s, treating connection as stale",
                heartbeat_timeout.as_secs()
            );
        }
    }
}

fn forward_tick(
    tick_tx: &mpsc::Sender<(FeedKey, LatestTick)>,
    key: FeedKey,
    tick: LatestTick,
) -> anyhow::Result<()> {
    if let Err(e) = tick_tx.try_send((key, tick)) {
        match e {
            mpsc::error::TrySendError::Full(_) => {
                tracing::debug!("failed to send tick: drain channel full");
            }
            mpsc::error::TrySendError::Closed(_) => {
                tracing::error!("failed to send tick: drain channel closed, stopping feed");
                return Err(anyhow::anyhow!("failed to send tick: drain channel closed"));
            }
        }
    }
    Ok(())
}

fn decode_ticker(msg: Message) -> Option<TickerMessage> {
    let Message::Text(text) = msg else {
        return None;
    };
    let rpc: JsonRpcMessage = serde_json::from_str(&text).ok()?;
    if rpc.method.as_deref() != Some("channelMessage") {
        return None;
    }
    Some(rpc.params?.message)
}

#[allow(clippy::too_many_arguments)]
async fn handle_ticker(
    ticker: TickerMessage,
    pool: Option<&PgPool>,
    tick_tx: &mpsc::Sender<(FeedKey, LatestTick)>,
    price_tx: &mpsc::Sender<PriceEvent>,
    builders: &mut HashMap<String, CandleBuilder>,
    h1_builders: &mut HashMap<String, CandleBuilder>,
    closes_map: &mut HashMap<String, Vec<Decimal>>,
    highs_map: &mut HashMap<String, Vec<Decimal>>,
    lows_map: &mut HashMap<String, Vec<Decimal>>,
    closes_map_h1: &mut HashMap<String, Vec<Decimal>>,
    highs_map_h1: &mut HashMap<String, Vec<Decimal>>,
    lows_map_h1: &mut HashMap<String, Vec<Decimal>>,
    latest_indicators: &mut HashMap<String, HashMap<String, Decimal>>,
    primary_tf_prefix: &str,
) -> anyhow::Result<bool> {
    let product_code = &ticker.product_code;

    let price = ticker.ltp;
    let size = ticker.volume;
    let best_bid = Some(ticker.best_bid);
    let best_ask = Some(ticker.best_ask);
    let ts = chrono::DateTime::parse_from_rfc3339(&ticker.timestamp)?.with_timezone(&chrono::Utc);

    forward_tick(
        tick_tx,
        FeedKey::new(Exchange::BitflyerCfd, Pair::new(product_code)),
        LatestTick {
            price,
            best_bid,
            best_ask,
            ts,
        },
    )?;

    let Some(builder) = builders.get_mut(product_code) else {
        return Ok(true);
    };

    let from_tick = builder.on_tick(price, size, ts, best_bid, best_ask);
    let from_complete = builder.try_complete(ts, best_bid, best_ask);
    if let Some(candle) = from_tick.or(from_complete)
        && !process_primary_candle(
            candle,
            pool,
            closes_map,
            highs_map,
            lows_map,
            price_tx,
            latest_indicators,
        )
        .await
    {
        return Ok(false);
    }

    if let Some(h1_builder) = h1_builders.get_mut(product_code) {
        let h1_from_tick = h1_builder.on_tick(price, size, ts, best_bid, best_ask);
        let h1_from_complete = h1_builder.try_complete(ts, best_bid, best_ask);
        if let Some(h1_candle) = h1_from_tick.or(h1_from_complete)
            && !process_h1_candle(
                h1_candle,
                pool,
                closes_map_h1,
                highs_map_h1,
                lows_map_h1,
                price_tx,
                latest_indicators,
                product_code,
                primary_tf_prefix,
            )
            .await
        {
            return Ok(false);
        }
    }

    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn connect_and_stream(
    ws_url: &str,
    pairs: &[Pair],
    pool: Option<&PgPool>,
    tick_tx: &mpsc::Sender<(FeedKey, LatestTick)>,
    price_tx: &mpsc::Sender<PriceEvent>,
    builders: &mut HashMap<String, CandleBuilder>,
    h1_builders: &mut HashMap<String, CandleBuilder>,
    closes_map: &mut HashMap<String, Vec<Decimal>>,
    highs_map: &mut HashMap<String, Vec<Decimal>>,
    lows_map: &mut HashMap<String, Vec<Decimal>>,
    closes_map_h1: &mut HashMap<String, Vec<Decimal>>,
    highs_map_h1: &mut HashMap<String, Vec<Decimal>>,
    lows_map_h1: &mut HashMap<String, Vec<Decimal>>,
    primary_timeframe: &str,
) -> anyhow::Result<()> {
    let (ws, _) = connect_async(ws_url).await?;
    let (mut write, mut read) = ws.split();
    tracing::info!(ws_url = %ws_url, "bitflyer websocket connected");

    subscribe_to_pairs(&mut write, pairs).await?;

    let mut latest_indicators: HashMap<String, HashMap<String, Decimal>> = HashMap::new();
    let primary_tf_prefix = primary_timeframe.to_lowercase();

    const HEARTBEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

    loop {
        let Some(msg) = receive_next_message(&mut read, HEARTBEAT_TIMEOUT).await? else {
            return Ok(());
        };
        let Some(ticker) = decode_ticker(msg) else {
            continue;
        };

        let keep_running = handle_ticker(
            ticker,
            pool,
            tick_tx,
            price_tx,
            builders,
            h1_builders,
            closes_map,
            highs_map,
            lows_map,
            closes_map_h1,
            highs_map_h1,
            lows_map_h1,
            &mut latest_indicators,
            &primary_tf_prefix,
        )
        .await?;
        if !keep_running {
            return Ok(());
        }
    }
}
