//! Phase 3: paper account の維持率ロスカット判定テスト。
//!
//! `detect_liquidation_targets` が paper account について、
//! 維持率 `< threshold` で正しく全 trade_id を返すこと、
//! および live account / price 不在の account を skip することを確認する。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Candle, Direction, Exchange, Pair, Trade, TradeStatus};
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

async fn make_price_store(
    exchange: Exchange,
    pair: &str,
    bid: Decimal,
    ask: Decimal,
) -> Arc<PriceStore> {
    let feed_key = FeedKey::new(exchange, Pair::new(pair));
    let store = PriceStore::new(vec![feed_key.clone()]);
    store
        .update(
            feed_key,
            LatestTick {
                price: (bid + ask) / dec!(2),
                best_bid: Some(bid),
                best_ask: Some(ask),
                ts: Utc::now(),
            },
        )
        .await;
    store
}

fn make_event(exchange: Exchange, pair: &str, close: Decimal) -> PriceEvent {
    PriceEvent {
        pair: Pair::new(pair),
        exchange,
        timestamp: Utc::now(),
        candle: Candle {
            pair: Pair::new(pair),
            exchange,
            timeframe: "M5".to_string(),
            open: close,
            high: close,
            low: close,
            close,
            volume: Some(0),
            best_bid: None,
            best_ask: None,
            timestamp: Utc::now(),
        },
        indicators: HashMap::new(),
    }
}

fn make_trade(
    account_id: Uuid,
    exchange: Exchange,
    pair: &str,
    direction: Direction,
    entry: Decimal,
    qty: Decimal,
    leverage: Decimal,
) -> Trade {
    Trade {
        id: Uuid::new_v4(),
        account_id,
        strategy_name: "test_strategy".into(),
        pair: Pair::new(pair),
        exchange,
        direction,
        entry_price: entry,
        exit_price: None,
        stop_loss: dec!(0),
        take_profit: None,
        quantity: qty,
        leverage,
        fees: dec!(0),
        entry_at: Utc::now(),
        exit_at: None,
        pnl_amount: None,
        exit_reason: None,
        status: TradeStatus::Open,
        max_hold_until: None,
        exchange_position_id: None,
    }
}

fn levels() -> HashMap<Exchange, Decimal> {
    let mut m = HashMap::new();
    m.insert(Exchange::GmoFx, dec!(1.00));
    m.insert(Exchange::BitflyerCfd, dec!(0.50));
    m
}

/// trade を seed して margin_lock する共通 helper。
/// balance はその分 (entry * qty / leverage) 減る。
async fn seed_and_lock(pool: &sqlx::PgPool, trade: &Trade) {
    auto_trader_db::trades::insert_trade(pool, trade)
        .await
        .expect("insert_trade failed");
    let margin = (trade.entry_price * trade.quantity / trade.leverage)
        .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    let mut tx = pool.begin().await.unwrap();
    auto_trader_db::trades::lock_margin(&mut tx, trade.account_id, trade.id, margin)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn liquidation_fires_when_maintenance_drops_below_threshold(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "liq_below",
        "paper",
        "gmo_fx",
        "test_strategy",
        100_000,
    )
    .await;
    let trade = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(10000),
        dec!(25),
    );
    seed_and_lock(&pool, &trade).await;

    // balance after lock = 100k - 60k = 40k (free cash)。equity 計算は
    // free cash + lock 戻し (60k) + unrealized。threshold=1.00 を下回るには
    // equity < 60k、つまり unrealized < -40k → current < 146。
    // current=145 → unrealized=(145-150)*10000=-50000、equity=40k+60k-50k=50k、
    // ratio = 50k/60k ≈ 0.833 < 1.00 → fire
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY", dec!(145), dec!(145.1)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(145));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("liq_below".into()),
        account_type: Some("paper".into()),
    };
    let ctx = auto_trader::liquidation::LiquidationContext {
        pool: pool.clone(),
        price_store: ps,
        exchange_liquidation_levels: std::sync::Arc::new(levels()),
        live_forces_dry_run: false,
    };
    let targets =
        auto_trader::liquidation::detect_liquidation_targets(&ctx, &[owned], &event).await;
    assert_eq!(targets.len(), 1, "one account should liquidate");
    assert_eq!(targets[0].0, account_id);
    assert_eq!(targets[0].1.len(), 1, "single trade in account");
}

#[sqlx::test(migrations = "../../migrations")]
async fn liquidation_does_not_fire_above_threshold(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "liq_above",
        "paper",
        "gmo_fx",
        "test_strategy",
        100_000,
    )
    .await;
    let trade = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(10000),
        dec!(25),
    );
    seed_and_lock(&pool, &trade).await;

    // current=200 → unrealized=+500000、equity=40k+60k+500k=600k、ratio=600k/60k=10.0 > 1.00
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY", dec!(200), dec!(200.1)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(200));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("liq_above".into()),
        account_type: Some("paper".into()),
    };
    let ctx = auto_trader::liquidation::LiquidationContext {
        pool: pool.clone(),
        price_store: ps,
        exchange_liquidation_levels: std::sync::Arc::new(levels()),
        live_forces_dry_run: false,
    };
    let targets =
        auto_trader::liquidation::detect_liquidation_targets(&ctx, &[owned], &event).await;
    assert!(
        targets.is_empty(),
        "no liquidation when ratio above threshold"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_account_skips_liquidation_judgment(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "liq_live",
        "live",
        "gmo_fx",
        "test_strategy",
        100_000,
    )
    .await;
    let trade = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(10000),
        dec!(25),
    );
    seed_and_lock(&pool, &trade).await;

    // current=145 → paper なら fire 条件だが live なので skip される
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY", dec!(145), dec!(145.1)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(145));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("liq_live".into()),
        account_type: Some("live".into()),
    };
    let ctx = auto_trader::liquidation::LiquidationContext {
        pool: pool.clone(),
        price_store: ps,
        exchange_liquidation_levels: std::sync::Arc::new(levels()),
        live_forces_dry_run: false, // live: skip
    };
    let targets =
        auto_trader::liquidation::detect_liquidation_targets(&ctx, &[owned], &event).await;
    assert!(
        targets.is_empty(),
        "live account must not be liquidated by paper logic"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn missing_price_skips_judgment(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "liq_missing_price",
        "paper",
        "gmo_fx",
        "test_strategy",
        100_000,
    )
    .await;
    let trade = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(10000),
        dec!(25),
    );
    seed_and_lock(&pool, &trade).await;

    // PriceStore は EUR_USD だけ持つ → USD_JPY の price 不在
    let ps = make_price_store(Exchange::GmoFx, "EUR_USD", dec!(1.0), dec!(1.001)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(148));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("liq_missing_price".into()),
        account_type: Some("paper".into()),
    };
    let ctx = auto_trader::liquidation::LiquidationContext {
        pool: pool.clone(),
        price_store: ps,
        exchange_liquidation_levels: std::sync::Arc::new(levels()),
        live_forces_dry_run: false,
    };
    let targets =
        auto_trader::liquidation::detect_liquidation_targets(&ctx, &[owned], &event).await;
    assert!(
        targets.is_empty(),
        "missing price must skip judgment (false-positive prevention)"
    );
}
