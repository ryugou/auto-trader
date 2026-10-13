use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use auto_trader_core::margin::OpenPosition;
use auto_trader_core::types::{Direction, Trade};
use auto_trader_market::price_store::{FeedKey, PriceStore};
use rust_decimal::Decimal;

type BoxedFutureAvoidingHrtbSendBug<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub fn build_close_side_positions<'a, I>(
    trades: I,
    price_store: &'a PriceStore,
    cache: &'a mut HashMap<FeedKey, Option<(Decimal, Decimal)>>,
    ctx_label: &'a str,
) -> BoxedFutureAvoidingHrtbSendBug<'a, Option<Vec<OpenPosition>>>
where
    I: IntoIterator<Item = &'a Trade> + Send + 'a,
    I::IntoIter: Send,
{
    Box::pin(async move {
        let mut positions = Vec::new();
        for trade in trades {
            let feed_key = FeedKey::new(trade.exchange, trade.pair.clone());
            let bid_ask = if let Some(cached) = cache.get(&feed_key) {
                *cached
            } else {
                let v = price_store.latest_bid_ask(&feed_key).await;
                cache.insert(feed_key.clone(), v);
                v
            };
            let current_price = match bid_ask {
                Some((bid, ask)) => match trade.direction {
                    Direction::Long => bid,
                    Direction::Short => ask,
                },
                None => {
                    tracing::warn!(
                        context = %ctx_label,
                        exchange = %trade.exchange,
                        pair = %trade.pair,
                        "no price available, skipping"
                    );
                    return None;
                }
            };
            positions.push(OpenPosition {
                direction: trade.direction,
                entry_price: trade.entry_price,
                current_price,
                quantity: trade.quantity,
                leverage: trade.leverage,
            });
        }
        Some(positions)
    })
}

#[cfg(test)]
mod tests;
