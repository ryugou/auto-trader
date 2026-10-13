use auto_trader_core::types::{Exchange, Pair, TradeStatus};
use auto_trader_market::price_store::{LatestTick, PriceStore};
use chrono::Utc;
use rust_decimal_macros::dec;
use uuid::Uuid;

use super::*;

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
    assert_eq!(positions[0].current_price, dec!(101));
    assert_eq!(positions[1].current_price, dec!(102));
}

#[tokio::test]
async fn missing_price_returns_none() {
    let feed_key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY"));
    let price_store = PriceStore::new(vec![feed_key]);

    let trade = make_trade(Exchange::BitflyerCfd, "FX_BTC_JPY", Direction::Long);
    let trades = [trade];

    let mut cache = HashMap::new();
    let positions =
        build_close_side_positions(trades.iter(), &price_store, &mut cache, "test").await;

    assert!(positions.is_none());
}
