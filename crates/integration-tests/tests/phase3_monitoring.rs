//! Phase 3C: Position monitoring — SL hit, TP hit, time limit close.
//!
//! DB + open trade + PriceStore + Trader.close_position() で
//! ポジション監視ロジックの結果を検証する。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Signal, TradeStatus};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::{Duration, Utc};
use rust_decimal_macros::dec;
use uuid::Uuid;

/// PriceStore を指定した bid/ask で構築する。
async fn make_price_store(
    exchange: Exchange,
    pair: &str,
    bid: rust_decimal::Decimal,
    ask: rust_decimal::Decimal,
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

/// dry_run Trader を構築する。
fn make_trader(
    pool: sqlx::PgPool,
    exchange: Exchange,
    account_id: Uuid,
    price_store: Arc<PriceStore>,
) -> Trader {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let sizer = Arc::new(PositionSizer::new(min_sizes));
    let api = MockExchangeApiBuilder::new().build();
    let notifier = Arc::new(Notifier::new_disabled());

    Trader::new(
        pool,
        exchange,
        account_id,
        "test_monitor".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        true,
    )
}

fn make_signal(pair: &str, direction: Direction) -> Signal {
    Signal {
        strategy_name: "test_strategy".to_string(),
        pair: Pair::new(pair),
        direction,
        stop_loss_pct: dec!(0.02),
        take_profit_pct: Some(dec!(0.04)),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        max_hold_until: None,
    }
}

// =========================================================================
// SL hit tests
// =========================================================================

/// SL hit Long: entry 時 bid=150/ask=151 (fill at 151), SL = 151*(1-0.02) = 147.98
/// close 時の bid を 147 に設定 → SL ヒットとして close_position を呼ぶ。
#[sqlx::test(migrations = "../../migrations")]
async fn sl_hit_long(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool, "sl_test", "paper", "gmo_fx", "test_strategy", 1_000_000,
    )
    .await;

    // entry: bid=150, ask=151
    let entry_ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, entry_ps.clone());

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    assert_eq!(trade.entry_price, dec!(151));
    // qty: balance=1_000_000, lev=2, Y=1.00, SL=0.02, alloc=1.0, entry=151 (Long@ask), min_lot=1
    //      max_alloc = 1/1.04, raw = 1_000_000 × 2 × (1/1.04) / 151 ≈ 12735.39 → 12735
    assert_eq!(trade.quantity, dec!(12735), "sizer: 1M × 2 × (1/1.04) / 151 → 12735");

    // SL = 151 * (1 - 0.02) = 147.98
    // 価格が SL を下回った: bid=147, ask=148
    // Position monitor が SL ヒットと判断し close_position を呼ぶ
    let feed_key = FeedKey::new(exchange, Pair::new("USD_JPY"));
    entry_ps
        .update(
            feed_key,
            LatestTick {
                price: dec!(147.5),
                best_bid: Some(dec!(147)),
                best_ask: Some(dec!(148)),
                ts: Utc::now(),
            },
        )
        .await;

    // Verify SL condition: for Long, current bid <= stop_loss
    let current_bid = dec!(147);
    assert!(
        current_bid <= trade.stop_loss,
        "SL Long condition: bid {current_bid} should be <= stop_loss {}",
        trade.stop_loss
    );

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit)
        .await
        .expect("close should succeed");

    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(closed.exit_reason, Some(ExitReason::SlHit));
    // Long close → bid price
    assert_eq!(closed.exit_price, Some(dec!(147)));
    // PnL = (147 - 151) * quantity → negative
    let pnl = closed.pnl_amount.expect("pnl should be set");
    assert!(pnl < dec!(0), "SL hit Long should have negative PnL, got {pnl}");
}

/// SL hit Short: entry 時 bid=150/ask=151 (fill at 150), SL = 150*(1+0.02) = 153
/// close 時の ask を 154 に設定 → SL ヒットとして close_position を呼ぶ。
#[sqlx::test(migrations = "../../migrations")]
async fn sl_hit_short(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool, "sl_test", "paper", "gmo_fx", "test_strategy", 1_000_000,
    )
    .await;

    let entry_ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, entry_ps.clone());

    let signal = make_signal("USD_JPY", Direction::Short);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    assert_eq!(trade.entry_price, dec!(150));
    // qty: balance=1_000_000, lev=2, Y=1.00, SL=0.02, alloc=1.0, entry=150 (Short@bid), min_lot=1
    //      max_alloc = 1/1.04, raw = 1_000_000 × 2 × (1/1.04) / 150 ≈ 12820.51 → 12820
    assert_eq!(trade.quantity, dec!(12820), "sizer: 1M × 2 × (1/1.04) / 150 → 12820");

    // SL = 150 * (1 + 0.02) = 153
    // 価格が SL を上回った: bid=153, ask=154
    let feed_key = FeedKey::new(exchange, Pair::new("USD_JPY"));
    entry_ps
        .update(
            feed_key,
            LatestTick {
                price: dec!(153.5),
                best_bid: Some(dec!(153)),
                best_ask: Some(dec!(154)),
                ts: Utc::now(),
            },
        )
        .await;

    // Verify SL condition: for Short, current ask >= stop_loss
    let current_ask = dec!(154);
    assert!(
        current_ask >= trade.stop_loss,
        "SL Short condition: ask {current_ask} should be >= stop_loss {}",
        trade.stop_loss
    );

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit)
        .await
        .expect("close should succeed");

    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(closed.exit_reason, Some(ExitReason::SlHit));
    // Short close → ask price
    assert_eq!(closed.exit_price, Some(dec!(154)));
    let pnl = closed.pnl_amount.expect("pnl should be set");
    assert!(pnl < dec!(0), "SL hit Short should have negative PnL, got {pnl}");
}

// =========================================================================
// TP hit tests
// =========================================================================

/// TP hit Long: entry at 151, TP = 151*(1+0.04) = 157.04
/// close 時 bid=158 → TP ヒットとして close。
#[sqlx::test(migrations = "../../migrations")]
async fn tp_hit_long(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool, "tp_test", "paper", "gmo_fx", "test_strategy", 1_000_000,
    )
    .await;

    let entry_ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, entry_ps.clone());

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    // qty: 1M × 2 × (1/1.04) / 151 → 12735 (Long@ask=151)
    assert_eq!(trade.quantity, dec!(12735), "sizer: 1M × 2 × (1/1.04) / 151 → 12735");

    // 価格上昇: bid=158, ask=159
    let feed_key = FeedKey::new(exchange, Pair::new("USD_JPY"));
    entry_ps
        .update(
            feed_key,
            LatestTick {
                price: dec!(158.5),
                best_bid: Some(dec!(158)),
                best_ask: Some(dec!(159)),
                ts: Utc::now(),
            },
        )
        .await;

    // Verify TP condition: for Long, current bid >= take_profit
    let current_bid = dec!(158);
    let tp = trade.take_profit.expect("take_profit should be set");
    assert!(
        current_bid >= tp,
        "TP Long condition: bid {current_bid} should be >= take_profit {tp}",
    );

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");

    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(closed.exit_reason, Some(ExitReason::TpHit));
    assert_eq!(closed.exit_price, Some(dec!(158)));
    let pnl = closed.pnl_amount.expect("pnl should be set");
    assert!(pnl > dec!(0), "TP hit Long should have positive PnL, got {pnl}");
}

/// TP hit Short: entry at 150, TP = 150*(1-0.04) = 144
/// close 時 ask=143 → TP ヒットとして close。
#[sqlx::test(migrations = "../../migrations")]
async fn tp_hit_short(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool, "tp_test", "paper", "gmo_fx", "test_strategy", 1_000_000,
    )
    .await;

    let entry_ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, entry_ps.clone());

    let signal = make_signal("USD_JPY", Direction::Short);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    // qty: 1M × 2 × (1/1.04) / 150 → 12820 (Short@bid=150)
    assert_eq!(trade.quantity, dec!(12820), "sizer: 1M × 2 × (1/1.04) / 150 → 12820");

    // 価格下落: bid=142, ask=143
    let feed_key = FeedKey::new(exchange, Pair::new("USD_JPY"));
    entry_ps
        .update(
            feed_key,
            LatestTick {
                price: dec!(142.5),
                best_bid: Some(dec!(142)),
                best_ask: Some(dec!(143)),
                ts: Utc::now(),
            },
        )
        .await;

    // Verify TP condition: for Short, current ask <= take_profit
    let current_ask = dec!(143);
    let tp = trade.take_profit.expect("take_profit should be set");
    assert!(
        current_ask <= tp,
        "TP Short condition: ask {current_ask} should be <= take_profit {tp}",
    );

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");

    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(closed.exit_reason, Some(ExitReason::TpHit));
    assert_eq!(closed.exit_price, Some(dec!(143)));
    let pnl = closed.pnl_amount.expect("pnl should be set");
    assert!(pnl > dec!(0), "TP hit Short should have positive PnL, got {pnl}");
}

// =========================================================================
// Time limit test
// =========================================================================

/// Time limit: max_hold_until が過去 → StrategyTimeLimit で close。
#[sqlx::test(migrations = "../../migrations")]
async fn time_limit_closes_expired_trade(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool, "time_test", "paper", "gmo_fx", "test_strategy", 1_000_000,
    )
    .await;

    let entry_ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, entry_ps.clone());

    // max_hold_until を過去に設定したシグナル
    let signal = Signal {
        strategy_name: "test_strategy".to_string(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        stop_loss_pct: dec!(0.02),
        take_profit_pct: Some(dec!(0.04)),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        max_hold_until: Some(Utc::now() - Duration::hours(1)),
    };
    let trade = trader.execute(&signal).await.expect("open should succeed");
    // qty: 1M × 2 × (1/1.04) / 151 → 12735 (Long@ask=151)
    assert_eq!(trade.quantity, dec!(12735), "sizer: 1M × 2 × (1/1.04) / 151 → 12735");
    // max_hold_until は過去 → position monitor が StrategyTimeLimit で close を発行
    assert!(trade.max_hold_until.is_some());
    let max_hold = trade.max_hold_until.unwrap();
    assert!(max_hold < Utc::now(), "max_hold_until should be in the past");

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::StrategyTimeLimit)
        .await
        .expect("close should succeed");

    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(closed.exit_reason, Some(ExitReason::StrategyTimeLimit));
    // Long close at bid=150
    assert_eq!(closed.exit_price, Some(dec!(150)));
}
