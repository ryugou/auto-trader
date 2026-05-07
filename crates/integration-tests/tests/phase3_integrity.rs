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
use auto_trader_integration_tests::helpers::db::{read_current_balance, seed_trading_account};
use auto_trader_integration_tests::helpers::sizing_invariants;
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

    let balance_before_open = read_current_balance(&pool, account_id).await;
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    let entry_price = trade.entry_price; // 151
    let quantity = trade.quantity;
    // qty: balance=1_000_000, lev=2, Y=1.00, SL=0.02, alloc=1.0, entry=151 (Long@ask), min_lot=1
    //      max_alloc = 1/1.04, raw = 1_000_000 × 2 × (1/1.04) / 151 ≈ 12735.39 → 12735
    assert_eq!(quantity, dec!(12735), "sizer: 1M × 2 × (1/1.04) / 151 → 12735");
    // Open-side enrichment.
    assert_eq!(
        trade.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            trade.entry_price,
            signal.direction,
            signal.stop_loss_pct,
        ),
    );
    assert_eq!(
        trade.take_profit,
        Some(sizing_invariants::expected_take_profit_price(
            trade.entry_price,
            signal.direction,
            signal.take_profit_pct.unwrap(),
        )),
    );
    assert_eq!(trade.leverage, dec!(2));
    assert_eq!(trade.fees, dec!(0));
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before_open,
        dec!(1.00),
    );

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

    // Close-side enrichment: explicit exit_reason / exit_price.
    assert_eq!(closed.exit_reason, Some(ExitReason::TpHit));
    assert_eq!(exit_price, dec!(155), "Long close fills at bid=155");

    // PnL = TRUNC((exit - entry) * qty, 0) = TRUNC((155 - 151) * qty, 0)
    let expected_pnl_raw = (exit_price - entry_price) * quantity;
    // truncate toward zero (JPY)
    let expected_pnl = expected_pnl_raw.round_dp_with_strategy(
        0,
        rust_decimal::RoundingStrategy::ToZero,
    );
    assert_eq!(pnl, expected_pnl, "PnL should be truncated to whole yen");
    // Cross-check via helper.
    let helper_pnl = sizing_invariants::expected_pnl(
        closed.entry_price,
        exit_price,
        closed.quantity,
        closed.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(pnl, helper_pnl, "pnl matches sizing_invariants::expected_pnl");

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
    let balance_before_t1 = read_current_balance(&pool, account_id).await;
    let signal1 = make_signal("USD_JPY", Direction::Long);
    let trade1 = trader.execute(&signal1).await.expect("open 1 should succeed");
    // qty: balance=10_000_000, lev=2, Y=1.00, SL=0.02, alloc=1.0, entry=151 (Long@ask), min_lot=1
    //      max_alloc = 1/1.04, raw = 10_000_000 × 2 / 1.04 / 151 ≈ 127356.087 → 127356
    assert_eq!(trade1.quantity, dec!(127356), "sizer: 10M × 2 × (1/1.04) / 151 → 127356");
    // Open-side enrichment for trade 1.
    assert_eq!(
        trade1.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            trade1.entry_price,
            signal1.direction,
            signal1.stop_loss_pct,
        ),
    );
    assert_eq!(
        trade1.take_profit,
        Some(sizing_invariants::expected_take_profit_price(
            trade1.entry_price,
            signal1.direction,
            signal1.take_profit_pct.unwrap(),
        )),
    );
    assert_eq!(trade1.leverage, dec!(2));
    assert_eq!(trade1.fees, dec!(0));
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade1,
        balance_before_t1,
        dec!(1.00),
    );

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
    // Close-side enrichment for trade 1.
    assert_eq!(closed1.exit_reason, Some(ExitReason::TpHit));
    let exit_price1 = closed1.exit_price.expect("trade1 exit_price must be set");
    assert_eq!(exit_price1, dec!(155), "Long close fills at bid=155");
    let expected_pnl1 = sizing_invariants::expected_pnl(
        closed1.entry_price,
        exit_price1,
        closed1.quantity,
        closed1.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(closed1.pnl_amount, Some(expected_pnl1));

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

    let balance_before_t2 = read_current_balance(&pool, account_id).await;
    let signal2 = make_signal("USD_JPY", Direction::Short);
    let trade2 = trader.execute(&signal2).await.expect("open 2 should succeed");
    // qty: balance has grown by trade1 PnL (+509424 yen: (155-151)×127356) → ≈10_509_424
    //      lev=2, Y=1.00, SL=0.02, alloc=1.0, entry=155 (Short@bid), min_lot=1
    //      max_alloc = 1/1.04, raw = 10_509_424 × 2 × (1/1.04) / 155 ≈ 130389 → 130389
    assert_eq!(trade2.quantity, dec!(130389), "sizer: ~10.51M × 2 × (1/1.04) / 155 → 130389 (balance grew by trade1 PnL)");
    // Open-side enrichment for trade 2.
    assert_eq!(
        trade2.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            trade2.entry_price,
            signal2.direction,
            signal2.stop_loss_pct,
        ),
    );
    assert_eq!(
        trade2.take_profit,
        Some(sizing_invariants::expected_take_profit_price(
            trade2.entry_price,
            signal2.direction,
            signal2.take_profit_pct.unwrap(),
        )),
    );
    assert_eq!(trade2.leverage, dec!(2));
    assert_eq!(trade2.fees, dec!(0));
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade2,
        balance_before_t2,
        dec!(1.00),
    );

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
    // Close-side enrichment for trade 2.
    assert_eq!(closed2.exit_reason, Some(ExitReason::SlHit));
    let exit_price2 = closed2.exit_price.expect("trade2 exit_price must be set");
    // Short close fills at ask=161 (price update to bid=160/ask=161 above).
    assert_eq!(exit_price2, dec!(161), "Short close fills at ask=161");
    let expected_pnl2 = sizing_invariants::expected_pnl(
        closed2.entry_price,
        exit_price2,
        closed2.quantity,
        closed2.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(closed2.pnl_amount, Some(expected_pnl2));

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

    let balance_before_open = read_current_balance(&pool, account_id).await;
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    let entry_price = trade.entry_price;
    let quantity = trade.quantity;
    // qty: balance=1_000_000, lev=2, Y=1.00, SL=0.02, alloc=1.0, entry=150.777 (Long@ask), min_lot=1
    //      max_alloc = 1/1.04, raw = 1_000_000 × 2 × (1/1.04) / 150.777 ≈ 12754.42 → 12754
    assert_eq!(quantity, dec!(12754), "sizer: 1M × 2 × (1/1.04) / 150.777 → 12754");
    // Open-side enrichment.
    assert_eq!(
        trade.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            trade.entry_price,
            signal.direction,
            signal.stop_loss_pct,
        ),
    );
    assert_eq!(
        trade.take_profit,
        Some(sizing_invariants::expected_take_profit_price(
            trade.entry_price,
            signal.direction,
            signal.take_profit_pct.unwrap(),
        )),
    );
    assert_eq!(trade.leverage, dec!(2));
    assert_eq!(trade.fees, dec!(0));
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before_open,
        dec!(1.00),
    );

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

    // Close-side enrichment.
    assert_eq!(closed.exit_reason, Some(ExitReason::TpHit));
    assert_eq!(exit_price, dec!(151.999), "Long close fills at bid=151.999");

    // PnL should be truncated to 0 decimal places (toward zero)
    let raw_pnl = (exit_price - entry_price) * quantity;
    let expected_pnl = raw_pnl.round_dp_with_strategy(
        0,
        rust_decimal::RoundingStrategy::ToZero,
    );
    assert_eq!(pnl, expected_pnl, "PnL should be truncated to whole yen");
    assert_eq!(pnl.scale(), 0, "PnL scale should be 0 (integer)");
    // Cross-check via helper.
    let helper_pnl = sizing_invariants::expected_pnl(
        closed.entry_price,
        exit_price,
        closed.quantity,
        closed.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(pnl, helper_pnl);
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

// =========================================================================
// 3.83: daily batch backfill — update_daily_max_drawdown
// =========================================================================

/// Insert closed trades for a past date, call update_daily_max_drawdown,
/// verify daily_summary rows exist with max_drawdown values.
#[sqlx::test(migrations = "../../migrations")]
async fn daily_batch_backfill(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "backfill_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    // Use a specific past date
    let target_date = chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
    let entry_at = target_date
        .and_hms_opt(10, 0, 0)
        .unwrap()
        .and_utc();
    let exit_at1 = target_date
        .and_hms_opt(11, 0, 0)
        .unwrap()
        .and_utc();
    let exit_at2 = target_date
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc();

    // Trade 1: win (+500)
    auto_trader_integration_tests::helpers::seed::seed_closed_trade(
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
        entry_at,
        exit_at1,
    )
    .await;

    // Trade 2: loss (-300)
    auto_trader_integration_tests::helpers::seed::seed_closed_trade(
        &pool,
        account_id,
        "test_strategy",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),
        dec!(147),
        dec!(-300),
        dec!(100),
        dec!(0),
        entry_at,
        exit_at2,
    )
    .await;

    // First rebuild the daily_summary so rows exist
    auto_trader_db::summary::rebuild_daily_summary(&pool, target_date)
        .await
        .expect("rebuild should succeed");

    // Then compute max drawdown
    auto_trader_db::summary::update_daily_max_drawdown(&pool, target_date)
        .await
        .expect("update_daily_max_drawdown should succeed");

    // Verify daily_summary row exists
    let rows: Vec<(i32, i32, Decimal, Decimal)> = sqlx::query_as(
        r#"SELECT trade_count, win_count, total_pnl, max_drawdown
           FROM daily_summary
           WHERE date = $1
             AND strategy_name = 'test_strategy'
             AND pair = 'USD_JPY'
             AND account_id = $2"#,
    )
    .bind(target_date)
    .bind(account_id)
    .fetch_all(&pool)
    .await
    .expect("query should succeed");

    assert_eq!(rows.len(), 1, "should have exactly 1 daily_summary row");
    let (trade_count, win_count, total_pnl, _max_drawdown) = &rows[0];
    assert_eq!(*trade_count, 2, "trade_count should be 2");
    assert_eq!(*win_count, 1, "win_count should be 1 (one profitable trade)");
    assert_eq!(*total_pnl, dec!(200), "total_pnl should be 500 - 300 = 200");
}

// =========================================================================
// 3.84: account_events — margin_lock/margin_release events
// =========================================================================

/// Execute a full open → close cycle and verify account_events rows exist.
#[sqlx::test(migrations = "../../migrations")]
async fn account_events_margin_lock_release(pool: sqlx::PgPool) {
    let exchange = Exchange::GmoFx;
    let account_id = seed_trading_account(
        &pool,
        "events_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;

    let ps = make_price_store(exchange, "USD_JPY", dec!(150), dec!(151)).await;
    let trader = make_trader(pool.clone(), exchange, account_id, ps);

    let balance_before_open = read_current_balance(&pool, account_id).await;
    // Open a trade
    let signal = make_signal("USD_JPY", Direction::Long);
    let trade = trader.execute(&signal).await.expect("open should succeed");
    // qty: 1M × 2 × (1/1.04) / 151 → 12735 (Long@ask=151)
    assert_eq!(trade.quantity, dec!(12735), "sizer: 1M × 2 × (1/1.04) / 151 → 12735");
    // Open-side enrichment.
    assert_eq!(
        trade.stop_loss,
        sizing_invariants::expected_stop_loss_price(
            trade.entry_price,
            signal.direction,
            signal.stop_loss_pct,
        ),
    );
    assert_eq!(
        trade.take_profit,
        Some(sizing_invariants::expected_take_profit_price(
            trade.entry_price,
            signal.direction,
            signal.take_profit_pct.unwrap(),
        )),
    );
    assert_eq!(trade.leverage, dec!(2));
    assert_eq!(trade.fees, dec!(0));
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before_open,
        dec!(1.00),
    );

    // Verify margin_lock event exists
    let lock_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_events WHERE trade_id = $1 AND event_type = 'margin_lock'",
    )
    .bind(trade.id)
    .fetch_one(&pool)
    .await
    .expect("query should succeed");
    assert_eq!(lock_count, 1, "margin_lock event should exist after open");

    // Verify the lock amount is negative (outflow)
    let lock_amount: Decimal = sqlx::query_scalar(
        "SELECT amount FROM account_events WHERE trade_id = $1 AND event_type = 'margin_lock'",
    )
    .bind(trade.id)
    .fetch_one(&pool)
    .await
    .expect("query should succeed");
    assert!(
        lock_amount < dec!(0),
        "margin_lock amount should be negative (outflow), got {}",
        lock_amount
    );

    // Close the trade
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit)
        .await
        .expect("close should succeed");
    // Close-side enrichment.
    assert_eq!(closed.exit_reason, Some(ExitReason::TpHit));
    let exit_price = closed.exit_price.expect("exit_price must be set");
    assert_eq!(exit_price, dec!(150), "Long close fills at bid=150");
    let expected_pnl = sizing_invariants::expected_pnl(
        closed.entry_price,
        exit_price,
        closed.quantity,
        closed.direction,
    )
    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
    assert_eq!(closed.pnl_amount, Some(expected_pnl));
    let balance_after_close = read_current_balance(&pool, account_id).await;
    assert_eq!(
        balance_after_close,
        balance_before_open + expected_pnl - closed.fees,
    );

    // Verify margin_release event exists
    let release_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_events WHERE trade_id = $1 AND event_type = 'margin_release'",
    )
    .bind(trade.id)
    .fetch_one(&pool)
    .await
    .expect("query should succeed");
    assert_eq!(
        release_count, 1,
        "margin_release event should exist after close"
    );

    // Verify trade_close event exists
    let close_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_events WHERE trade_id = $1 AND event_type = 'trade_close'",
    )
    .bind(trade.id)
    .fetch_one(&pool)
    .await
    .expect("query should succeed");
    assert_eq!(
        close_count, 1,
        "trade_close event should exist after close"
    );
}

// =========================================================================
// 3.85: entry_indicators / regime — JSONB storage
// =========================================================================

/// Insert a trade with entry_indicators JSONB, verify it's stored and retrievable.
#[sqlx::test(migrations = "../../migrations")]
async fn entry_indicators_jsonb_stored(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "indicators_test",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
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

    // Write entry_indicators JSONB
    let indicators = serde_json::json!({
        "regime": "trending",
        "atr": 1.5,
        "rsi": 65.2,
        "bb_position": "above_mid",
        "adx": 30.0
    });

    sqlx::query("UPDATE trades SET entry_indicators = $1 WHERE id = $2")
        .bind(&indicators)
        .bind(trade_id)
        .execute(&pool)
        .await
        .expect("update should succeed");

    // Read it back
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT entry_indicators FROM trades WHERE id = $1")
            .bind(trade_id)
            .fetch_one(&pool)
            .await
            .expect("query should succeed");

    assert_eq!(
        stored.get("regime").and_then(|v| v.as_str()),
        Some("trending"),
        "regime should be stored as 'trending'"
    );
    assert_eq!(
        stored.get("atr").and_then(|v| v.as_f64()),
        Some(1.5),
        "atr should be stored as 1.5"
    );
    assert_eq!(
        stored.get("rsi").and_then(|v| v.as_f64()),
        Some(65.2),
        "rsi should be stored"
    );
    assert_eq!(
        stored.get("bb_position").and_then(|v| v.as_str()),
        Some("above_mid"),
        "bb_position should be stored"
    );
}

// =========================================================================
// 3.99: PriceStore mid() — returns correct mid price
// =========================================================================

/// PriceStore::mid() returns (bid + ask) / 2 when both are present.
#[tokio::test]
async fn price_store_mid_with_bid_ask() {
    use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};

    let pair = Pair::new("USD_JPY");
    let feed_key = FeedKey::new(Exchange::GmoFx, pair.clone());
    let store = PriceStore::new(vec![feed_key.clone()]);

    store
        .update(
            feed_key,
            LatestTick {
                price: dec!(150),
                best_bid: Some(dec!(149)),
                best_ask: Some(dec!(151)),
                ts: Utc::now(),
            },
        )
        .await;

    let mid = store.mid(&pair).await;
    assert_eq!(
        mid,
        Some(dec!(150)),
        "mid should be (149 + 151) / 2 = 150"
    );
}

/// PriceStore::mid() falls back to LTP when bid/ask are absent.
#[tokio::test]
async fn price_store_mid_fallback_to_ltp() {
    use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};

    let pair = Pair::new("USD_JPY");
    let feed_key = FeedKey::new(Exchange::Oanda, pair.clone());
    let store = PriceStore::new(vec![feed_key.clone()]);

    store
        .update(
            feed_key,
            LatestTick {
                price: dec!(150),
                best_bid: None,
                best_ask: None,
                ts: Utc::now(),
            },
        )
        .await;

    let mid = store.mid(&pair).await;
    assert_eq!(
        mid,
        Some(dec!(150)),
        "mid should fall back to LTP (150) when bid/ask are absent"
    );
}

/// PriceStore::mid() returns None for unknown pair.
#[tokio::test]
async fn price_store_mid_returns_none_for_unknown() {
    use auto_trader_market::price_store::PriceStore;

    let store = PriceStore::new(vec![]);
    let mid = store.mid(&Pair::new("UNKNOWN_PAIR")).await;
    assert!(mid.is_none(), "mid should return None for unknown pair");
}
