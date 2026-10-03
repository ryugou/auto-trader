//! open trade 群から close-side (Long=bid / Short=ask) の OpenPosition 列を
//! 組み立てる共通ヘルパ。維持率計算 (liquidation / margin_alert) と
//! equity 照合 (balance_drift) が同じ不変条件を共有する。
//! price が 1 つでも欠けたら None (呼び出し側は口座ごと skip して
//! false-positive を避ける)。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use auto_trader_core::margin::OpenPosition;
use auto_trader_core::types::{Direction, Trade};
use auto_trader_market::price_store::{FeedKey, PriceStore};
use rust_decimal::Decimal;

/// `cache` は同一 tick 内の FeedKey 別 bid/ask をメモ化する (tick 跨ぎで
/// 使い回さないこと)。one-shot 呼び出し (balance_drift) は毎回新品を渡す。
///
/// 戻り値を `Box::pin` した `dyn Future` にしているのは意図的:
/// `async fn` のまま `impl IntoIterator<Item = &'a Trade>` を引数に取ると、
/// 呼び出し元 (`detect_liquidation_targets` 等) をさらに `tokio::spawn` する
/// 経路で "implementation of `Send` is not general enough" という rustc の
/// HRTB 推論の既知の偽陽性 (opaque future 型に生の lifetime パラメータが
/// 漏れて `for<'a> Send` を要求されてしまう) に当たる。`dyn Future + Send`
/// に一度落とすと具体的な trait object 境界になり回避できる。
pub fn build_close_side_positions<'a, I>(
    trades: I,
    price_store: &'a PriceStore,
    cache: &'a mut HashMap<FeedKey, Option<(Decimal, Decimal)>>,
    ctx_label: &'a str,
) -> Pin<Box<dyn Future<Output = Option<Vec<OpenPosition>>> + Send + 'a>>
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
                    // close-side: Long は bid で決済, Short は ask で決済。
                    Direction::Long => bid,
                    Direction::Short => ask,
                },
                None => {
                    tracing::warn!(
                        "{ctx_label}: no price for {:?} {} — skipping",
                        trade.exchange,
                        trade.pair
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
mod tests {
    use super::*;
    use auto_trader_core::types::{Exchange, Pair, TradeStatus};
    use auto_trader_market::price_store::{LatestTick, PriceStore};
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    fn make_trade(exchange: Exchange, pair: &str, direction: Direction) -> Trade {
        Trade {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            strategy_name: "test".to_string(),
            pair: Pair::new(pair),
            exchange,
            direction,
            entry_price: dec!(100),
            exit_price: None,
            stop_loss: dec!(90),
            take_profit: None,
            quantity: dec!(1),
            leverage: dec!(1),
            fees: dec!(0),
            entry_at: Utc::now(),
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: None,
            exchange_position_id: None,
            stop_order_id: None,
        }
    }

    #[tokio::test]
    async fn all_prices_present_returns_close_side_positions() {
        let feed_key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY"));
        let price_store = PriceStore::new(vec![feed_key.clone()]);
        price_store
            .update(
                feed_key,
                LatestTick {
                    price: dec!(101.5),
                    best_bid: Some(dec!(101)),
                    best_ask: Some(dec!(102)),
                    ts: Utc::now(),
                },
            )
            .await;

        let long_trade = make_trade(Exchange::BitflyerCfd, "FX_BTC_JPY", Direction::Long);
        let short_trade = make_trade(Exchange::BitflyerCfd, "FX_BTC_JPY", Direction::Short);
        let trades = [long_trade, short_trade];

        let mut cache = HashMap::new();
        let positions = build_close_side_positions(trades.iter(), &price_store, &mut cache, "test")
            .await
            .expect("should build positions");

        assert_eq!(positions.len(), 2);
        assert_eq!(positions[0].current_price, dec!(101)); // Long close = bid
        assert_eq!(positions[1].current_price, dec!(102)); // Short close = ask
    }

    #[tokio::test]
    async fn missing_price_returns_none() {
        let feed_key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY"));
        let price_store = PriceStore::new(vec![feed_key]);
        // no update — price stays unset

        let trade = make_trade(Exchange::BitflyerCfd, "FX_BTC_JPY", Direction::Long);
        let trades = [trade];

        let mut cache = HashMap::new();
        let positions =
            build_close_side_positions(trades.iter(), &price_store, &mut cache, "test").await;

        assert!(positions.is_none());
    }
}
