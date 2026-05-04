//! Phase 3D: Data integrity — balance after trade, daily_summary accuracy,
//! candle upsert dedup, JPY truncation, overnight fee.
//!
//! DB を使ったデータ整合性テスト。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Candle, Direction, Exchange, ExitReason, Pair, Signal};
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

    // Use the date from the last closed trade's exit_at to avoid UTC midnight flakiness
    let date = closed2.exit_at.unwrap().date_naive();
    auto_trader_db::summary::rebuild_daily_summary(&pool, date)
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
    .bind(date)
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

// =========================================================================
// 3.86: candle upsert dedup
// =========================================================================

/// 同一キャンドル (exchange, pair, timeframe, timestamp) を 2 回 upsert しても 1 行のみ。
#[sqlx::test(migrations = "../../migrations")]
async fn candle_upsert_dedup(pool: sqlx::PgPool) {
    let ts = chrono::Utc::now();
    let candle = Candle {
        pair: Pair::new("USD_JPY"),
        exchange: Exchange::GmoFx,
        timeframe: "M5".to_string(),
        open: dec!(150),
        high: dec!(151),
        low: dec!(149),
        close: dec!(150.5),
        volume: Some(100),
        best_bid: None,
        best_ask: None,
        timestamp: ts,
    };

    // Insert twice
    auto_trader_db::candles::upsert_candle(&pool, &candle)
        .await
        .expect("first upsert should succeed");
    auto_trader_db::candles::upsert_candle(&pool, &candle)
        .await
        .expect("second upsert should succeed");

    // Count rows
    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM price_candles
           WHERE exchange = $1 AND pair = $2 AND timeframe = $3 AND timestamp = $4"#,
    )
    .bind(candle.exchange.as_str())
    .bind(&candle.pair.0)
    .bind(&candle.timeframe)
    .bind(ts)
    .fetch_one(&pool)
    .await
    .expect("count query should succeed");

    assert_eq!(count, 1, "duplicate upsert should result in exactly 1 row");
}

/// upsert で値が更新されることを確認。
#[sqlx::test(migrations = "../../migrations")]
async fn candle_upsert_updates_values(pool: sqlx::PgPool) {
    let ts = chrono::Utc::now();
    let candle1 = Candle {
        pair: Pair::new("USD_JPY"),
        exchange: Exchange::GmoFx,
        timeframe: "M5".to_string(),
        open: dec!(150),
        high: dec!(151),
        low: dec!(149),
        close: dec!(150),
        volume: Some(100),
        best_bid: None,
        best_ask: None,
        timestamp: ts,
    };

    auto_trader_db::candles::upsert_candle(&pool, &candle1)
        .await
        .expect("first upsert");

    // Update with new close price
    let candle2 = Candle {
        close: dec!(152),
        high: dec!(153),
        ..candle1.clone()
    };

    auto_trader_db::candles::upsert_candle(&pool, &candle2)
        .await
        .expect("second upsert");

    let candles = auto_trader_db::candles::get_candles(
        &pool,
        "gmo_fx",
        "USD_JPY",
        "M5",
        10,
    )
    .await
    .expect("get_candles should succeed");

    assert_eq!(candles.len(), 1);
    assert_eq!(candles[0].close, dec!(152), "close should be updated");
    assert_eq!(candles[0].high, dec!(153), "high should be updated");
}

// =========================================================================
// 3.87: JPY truncation
// =========================================================================

/// PnL 計算が整数に切り捨て (TRUNC toward zero) されることを確認。
/// Trader.close_position の内部で truncate_yen が適用される。
#[sqlx::test(migrations = "../../migrations")]
async fn jpy_truncation_on_pnl(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let initial_balance: i64 = 1_000_000;
    let account_id = auto_trader_integration_tests::helpers::db::seed_trading_account(
        &pool,
        "trunc_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        initial_balance,
    )
    .await;

    // Entry: bid=150.333, ask=150.777 → Long fills at ask=150.777
    let ps = make_price_store(exchange, "USD_JPY", dec!(150.333), dec!(150.777)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, ps.clone());

    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    let entry_price = trade.entry_price;
    let quantity = trade.quantity;

    // Close: bid=151.999 → Long close at bid, diff = 151.999 - 150.777 = 1.222
    // pnl = 1.222 * quantity → truncated to integer
    let feed_key = FeedKey::new(exchange, Pair::new("USD_JPY"));
    ps.update(
        feed_key,
        LatestTick {
            price: dec!(152),
            best_bid: Some(dec!(151.999)),
            best_ask: Some(dec!(152.001)),
            ts: Utc::now(),
        },
    )
    .await;

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");

    let pnl = closed.pnl_amount.expect("pnl should be set");
    let exit_price = closed.exit_price.expect("exit_price should be set");

    // PnL should be truncated to 0 decimal places (toward zero)
    let raw_pnl = (exit_price - entry_price) * quantity;
    let expected_pnl = raw_pnl.round_dp_with_strategy(
        0,
        rust_decimal::RoundingStrategy::ToZero,
    );
    assert_eq!(pnl, expected_pnl, "PnL should be truncated to whole yen");
    assert_eq!(pnl.scale(), 0, "PnL scale should be 0 (integer)");
}

// =========================================================================
// 3.51-3.52: Overnight fee
// =========================================================================

/// apply_overnight_fee: open trade に手数料を適用 → balance 減少 + fees 増加。
#[sqlx::test(migrations = "../../migrations")]
async fn overnight_fee_applied_to_open_trade(pool: sqlx::PgPool) {
    let initial_balance: i64 = 1_000_000;
    let account_id = auto_trader_integration_tests::helpers::db::seed_trading_account(
        &pool,
        "fee_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        initial_balance,
    )
    .await;

    let trade_id = auto_trader_integration_tests::helpers::seed::seed_open_trade(
        &pool,
        account_id,
        "test_strategy",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(149),
        dec!(100),
        Utc::now(),
    )
    .await;

    let fee_amount = dec!(50);
    let mut tx = pool.begin().await.expect("begin tx");
    let result = auto_trader_db::trades::apply_overnight_fee(
        &mut tx, account_id, trade_id, fee_amount,
    )
    .await
    .expect("apply_overnight_fee should succeed");
    tx.commit().await.expect("commit");

    // Should return Some(new_balance)
    let new_balance = result.expect("fee should be applied to open trade");
    let expected_balance = Decimal::from(initial_balance) - fee_amount;
    assert_eq!(new_balance, expected_balance, "balance should decrease by fee");

    // Verify fees on trade
    let fees: Decimal = sqlx::query_scalar("SELECT fees FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_one(&pool)
        .await
        .expect("query should succeed");
    assert_eq!(fees, fee_amount, "trade fees should be incremented");

    // Verify account_events row
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_events WHERE trade_id = $1 AND event_type = 'overnight_fee'"
    )
    .bind(trade_id)
    .fetch_one(&pool)
    .await
    .expect("query should succeed");
    assert_eq!(event_count, 1, "should have 1 overnight_fee event");
}

/// apply_overnight_fee: closed trade には適用されない → Ok(None)。
#[sqlx::test(migrations = "../../migrations")]
async fn overnight_fee_skips_closed_trade(pool: sqlx::PgPool) {
    let account_id = auto_trader_integration_tests::helpers::db::seed_trading_account(
        &pool,
        "fee_skip_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let trade_id = auto_trader_integration_tests::helpers::seed::seed_closed_trade(
        &pool,
        account_id,
        "test_strategy",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(155),
        dec!(500),
        dec!(100),
        dec!(0),
        Utc::now() - chrono::Duration::hours(2),
        Utc::now(),
    )
    .await;

    let fee_amount = dec!(50);
    let mut tx = pool.begin().await.expect("begin tx");
    let result = auto_trader_db::trades::apply_overnight_fee(
        &mut tx, account_id, trade_id, fee_amount,
    )
    .await
    .expect("apply_overnight_fee should not error");
    tx.commit().await.expect("commit");

    assert!(
        result.is_none(),
        "overnight fee should be skipped for closed trade"
    );
}
