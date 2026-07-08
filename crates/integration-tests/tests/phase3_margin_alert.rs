//! Phase 3: live account の維持率アラート判定テスト。
//!
//! `detect_margin_alerts` が **live** account について維持率を warn/critical
//! に分類して返すこと、paper account は (liquidation.rs の担当なので) alert
//! しないこと、price 不在 account を skip することを確認する。close は一切
//! 行わない (live のロスカット執行は取引所の責務)。
//!
//! 維持率の式は `compute_maintenance_ratio` と同じ:
//!   ratio = (current_balance + Σrequired + Σunrealized) / Σrequired
//! seed=100k / trade(entry=150, qty=10000, lev=25) → required=60k,
//! lock 後 current_balance=40k なので ratio = (100000 + unrealized)/60000。
//! Y=1.00 のとき warn 帯 = [1.1, 1.3)、critical 帯 = (-∞, 1.1)。

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

use auto_trader::margin_alert::{AlertLevel, detect_margin_alerts};

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
        stop_order_id: None,
    }
}

fn levels() -> HashMap<Exchange, Decimal> {
    let mut m = HashMap::new();
    m.insert(Exchange::GmoFx, dec!(1.00));
    m.insert(Exchange::BitflyerCfd, dec!(0.50));
    m
}

/// trade を seed して margin_lock する共通 helper (balance がその分減る)。
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

/// live account を seed し、指定 current price で `detect_margin_alerts` を
/// 走らせて返す小さな共通 runner。
async fn run_for_price(
    pool: &sqlx::PgPool,
    account_type: &str,
    name: &str,
    current: Decimal,
) -> Vec<auto_trader::margin_alert::MarginAlert> {
    let account_id =
        seed_trading_account(pool, name, account_type, "gmo_fx", "test_strategy", 100_000).await;
    let trade = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(10000),
        dec!(25),
    );
    seed_and_lock(pool, &trade).await;

    // Long → close-side bid を current にする。
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY", current, current + dec!(0.1)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", current);

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some(name.into()),
        account_type: Some(account_type.into()),
    };
    let ctx = auto_trader::liquidation::LiquidationContext {
        pool: pool.clone(),
        price_store: ps,
        exchange_liquidation_levels: std::sync::Arc::new(levels()),
        live_forces_dry_run: false,
    };
    detect_margin_alerts(&ctx, &[owned], &event).await
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_account_warn_band(pool: sqlx::PgPool) {
    // current=147.2 → unrealized=-28000 → ratio=(100000-28000)/60000=1.2 → Warn (1.1<=1.2<1.3)
    let alerts = run_for_price(&pool, "live", "margin_warn", dec!(147.2)).await;
    assert_eq!(alerts.len(), 1, "one live account should warn");
    assert_eq!(alerts[0].level, AlertLevel::Warn);
    assert_eq!(alerts[0].threshold, dec!(1.00));
    assert_eq!(alerts[0].ratio, dec!(72000) / dec!(60000));
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_account_critical_band(pool: sqlx::PgPool) {
    // current=146.5 → unrealized=-35000 → ratio=(100000-35000)/60000≈1.0833 < 1.1 → Critical
    let alerts = run_for_price(&pool, "live", "margin_critical", dec!(146.5)).await;
    assert_eq!(alerts.len(), 1, "one live account should be critical");
    assert_eq!(alerts[0].level, AlertLevel::Critical);
    assert_eq!(alerts[0].ratio, dec!(65000) / dec!(60000));
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_account_healthy_no_alert(pool: sqlx::PgPool) {
    // current=150 → unrealized=0 → ratio=100000/60000≈1.667 >= 1.3 → None
    let alerts = run_for_price(&pool, "live", "margin_healthy", dec!(150)).await;
    assert!(
        alerts.is_empty(),
        "healthy live account (ratio >= Y×1.3) must not alert, got {alerts:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_account_not_alerted(pool: sqlx::PgPool) {
    // paper account を warn 帯の価格に置いても alert しない (liquidation.rs の担当)。
    let alerts = run_for_price(&pool, "paper", "margin_paper", dec!(147.2)).await;
    assert!(
        alerts.is_empty(),
        "paper account must not be margin-alerted (live-only), got {alerts:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn missing_price_skips_alert(pool: sqlx::PgPool) {
    // PriceStore が別 pair しか持たない → USD_JPY の price 不在で account skip。
    let account_id = seed_trading_account(
        &pool,
        "margin_missing_price",
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

    let ps = make_price_store(Exchange::GmoFx, "EUR_JPY", dec!(160.0), dec!(160.01)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(147));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("margin_missing_price".into()),
        account_type: Some("live".into()),
    };
    let ctx = auto_trader::liquidation::LiquidationContext {
        pool: pool.clone(),
        price_store: ps,
        exchange_liquidation_levels: std::sync::Arc::new(levels()),
        live_forces_dry_run: false,
    };
    let alerts = detect_margin_alerts(&ctx, &[owned], &event).await;
    assert!(
        alerts.is_empty(),
        "missing price must skip judgment (false-positive prevention)"
    );
}
