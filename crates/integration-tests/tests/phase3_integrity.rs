//! Phase 3D: Data integrity — balance after trade, daily_summary accuracy.
//!
//! DB を使ったデータ整合性テスト。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, Signal};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
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
        "test_integrity".to_string(),
        api,
        price_store,
        notifier,
        sizer,
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
// Balance after trade
// =========================================================================

/// 残高整合性: open → close 後の current_balance = initial + pnl - fees。
///
/// 1. initial_balance = 1,000,000
/// 2. Long open at ask=151: margin locked = floor(151 * qty / leverage)
///    → balance decreases by margin
/// 3. Close at bid=155 (profit)
///    → balance = balance + margin + pnl
/// 4. 最終 balance = initial + pnl (fees=0 in dry_run)
#[sqlx::test(migrations = "../../migrations")]
async fn balance_after_trade(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let initial_balance: i64 = 1_000_000;
    let account_id = seed_trading_account(
        &pool,
        "balance_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        initial_balance,
    )
    .await;

    // Entry: bid=150, ask=151
    let ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, ps.clone());

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    let entry_price = trade.entry_price; // 151
    let quantity = trade.quantity;

    // Close: bid=155, ask=156 → Long close at bid=155
    let feed_key = FeedKey::new(exchange, Pair::new("USD_JPY"));
    ps.update(
        feed_key,
        LatestTick {
            price: dec!(155.5),
            best_bid: Some(dec!(155)),
            best_ask: Some(dec!(156)),
            ts: Utc::now(),
        },
    )
    .await;

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");

    let exit_price = closed.exit_price.expect("exit_price should be set");
    let pnl = closed.pnl_amount.expect("pnl should be set");
    let fees = closed.fees;

    // PnL = TRUNC((exit - entry) * qty, 0) = TRUNC((155 - 151) * qty, 0)
    let expected_pnl_raw = (exit_price - entry_price) * quantity;
    // truncate toward zero (JPY)
    let expected_pnl = expected_pnl_raw.round_dp_with_strategy(
        0,
        rust_decimal::RoundingStrategy::ToZero,
    );
    assert_eq!(pnl, expected_pnl, "PnL should be truncated to whole yen");

    // Check DB balance
    let account = auto_trader_db::trading_accounts::get_account(&pool, account_id)
        .await
        .expect("get_account should succeed")
        .expect("account should exist");

    let expected_balance = Decimal::from(initial_balance) + pnl - fees;
    assert_eq!(
        account.current_balance, expected_balance,
        "current_balance should be initial + pnl - fees"
    );
}

// =========================================================================
// daily_summary accuracy
// =========================================================================

/// daily_summary 整合性: 複数トレードを close → rebuild_daily_summary → counts が一致。
#[sqlx::test(migrations = "../../migrations")]
async fn daily_summary_accuracy(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "summary_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        10_000_000,
    )
    .await;

    let ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, ps.clone());

    // Trade 1: Long, close with profit
    let signal1 = make_signal("USD_JPY", Direction::Long);
    let trade1 = trader.execute(&signal1).await.expect("open 1 should succeed");

    // Update price for profitable close
    let feed_key = FeedKey::new(exchange, Pair::new("USD_JPY"));
    ps.update(
        feed_key.clone(),
        LatestTick {
            price: dec!(155),
            best_bid: Some(dec!(155)),
            best_ask: Some(dec!(156)),
            ts: Utc::now(),
        },
    )
    .await;

    let closed1 = trader
        .close_position(&trade1.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close 1 should succeed");

    // Trade 2: Short, close with loss
    // Reset price for entry
    ps.update(
        feed_key.clone(),
        LatestTick {
            price: dec!(155.5),
            best_bid: Some(dec!(155)),
            best_ask: Some(dec!(156)),
            ts: Utc::now(),
        },
    )
    .await;

    let signal2 = make_signal("USD_JPY", Direction::Short);
    let trade2 = trader.execute(&signal2).await.expect("open 2 should succeed");

    // Price rises → Short loses
    ps.update(
        feed_key.clone(),
        LatestTick {
            price: dec!(160),
            best_bid: Some(dec!(160)),
            best_ask: Some(dec!(161)),
            ts: Utc::now(),
        },
    )
    .await;

    let closed2 = trader
        .close_position(&trade2.id.to_string(), ExitReason::SlHit)
        .await
        .expect("close 2 should succeed");

    // Rebuild daily summary for today
    let today = Utc::now().date_naive();
    auto_trader_db::summary::rebuild_daily_summary(&pool, today)
        .await
        .expect("rebuild should succeed");

    // Verify daily_summary
    let rows: Vec<(i32, i32, Decimal)> = sqlx::query_as(
        r#"SELECT trade_count, win_count, total_pnl
           FROM daily_summary
           WHERE date = $1
             AND strategy_name = 'test_strategy'
             AND pair = 'USD_JPY'
             AND account_id = $2"#,
    )
    .bind(today)
    .bind(account_id)
    .fetch_all(&pool)
    .await
    .expect("query should succeed");

    assert_eq!(rows.len(), 1, "should have exactly 1 daily_summary row");
    let (trade_count, win_count, total_pnl) = &rows[0];

    assert_eq!(*trade_count, 2, "trade_count should be 2");

    // Trade 1 was profitable (Long entry at 151, close at 155), Trade 2 was a loss
    let pnl1 = closed1.pnl_amount.unwrap();
    let pnl2 = closed2.pnl_amount.unwrap();

    let expected_wins = if pnl1 > dec!(0) { 1 } else { 0 } + if pnl2 > dec!(0) { 1 } else { 0 };
    assert_eq!(*win_count, expected_wins, "win_count should match profitable trades");

    let expected_total_pnl = pnl1 + pnl2;
    assert_eq!(*total_pnl, expected_total_pnl, "total_pnl should be sum of individual PnLs");
}
