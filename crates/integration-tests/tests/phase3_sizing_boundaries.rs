//! Phase 3 sizing boundary tests — verify `trader.execute` behaves
//! correctly at the edges of the broker-aware sizing formula introduced
//! by PR #80 (`max_alloc = 1 / (Y + leverage × stop_loss_pct)`).
//!
//! Each test wires up a minimal `Trader` (no `PipelineHarness`) and drives
//! a single `execute` call so the sizing-edge assertions remain isolated
//! and easy to reason about.

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, Pair, Signal};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::helpers::sizing_invariants;
use auto_trader_market::null_exchange_api::NullExchangeApi;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

// ─── Helpers ───────────────────────────────────────────────────────────────

fn make_signal(
    pair: &str,
    direction: Direction,
    sl_pct: Decimal,
    alloc_pct: Decimal,
) -> Signal {
    Signal {
        strategy_name: "boundary_strategy".to_string(),
        pair: Pair::new(pair),
        direction,
        stop_loss_pct: sl_pct,
        take_profit_pct: Some(sl_pct * dec!(2)),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: alloc_pct,
        max_hold_until: None,
    }
}

/// Override the seeded leverage (default 2 from `seed_trading_account`) so
/// gmo_fx boundary tests can run with the production-realistic 10x leverage.
async fn override_leverage(pool: &sqlx::PgPool, account_id: Uuid, leverage: Decimal) {
    sqlx::query("UPDATE trading_accounts SET leverage = $1 WHERE id = $2")
        .bind(leverage)
        .bind(account_id)
        .execute(pool)
        .await
        .expect("leverage update should succeed");
}

/// Read the current_balance for assertions in multi-execute scenarios.
async fn current_balance(pool: &sqlx::PgPool, account_id: Uuid) -> Decimal {
    let row: (Decimal,) =
        sqlx::query_as("SELECT current_balance FROM trading_accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(pool)
            .await
            .expect("read current_balance");
    row.0
}

/// Construct a `Trader` (dry_run = true) wired to a fresh `PriceStore`
/// already seeded with the requested bid/ask pair.
#[allow(clippy::too_many_arguments)]
async fn build_trader_with_market(
    pool: sqlx::PgPool,
    account_id: Uuid,
    exchange: Exchange,
    pair_str: &str,
    bid: Decimal,
    ask: Decimal,
    min_order_size: Decimal,
    liquidation_margin_level: Decimal,
) -> Trader {
    let pair = Pair::new(pair_str);
    let feed_key = FeedKey::new(exchange, pair.clone());
    let price_store = PriceStore::new(vec![feed_key.clone()]);
    price_store
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

    let mut min_sizes = HashMap::new();
    min_sizes.insert(pair, min_order_size);
    let sizer = Arc::new(PositionSizer::new(min_sizes));

    Trader::new(
        pool,
        exchange,
        account_id,
        "boundary_test_account".to_string(),
        Arc::new(NullExchangeApi),
        price_store,
        Arc::new(Notifier::new_disabled()),
        sizer,
        liquidation_margin_level,
        true, // dry_run
    )
}

// ─── Tests ─────────────────────────────────────────────────────────────────

/// **Test 1**: balance at the JPY minimum (10,000 yen) with BTC priced at
/// 25M (a deliberately stressed scenario) produces raw_qty = 0.0008 BTC,
/// which is below the 0.001 min_lot. The sizer must return None and
/// `trader.execute` must surface that as an error (not silently downgrade
/// the order).
///
/// We use balance=10,000 (the floor enforced by
/// `trading_accounts_jpy_min_balance`) and a 25M BTC quote so that
/// raw_qty = 10,000 × 2 × 1.0 / 25,000,000 = 0.0008 BTC, still under the
/// 0.001 min_lot — preserving the original "raw_qty < min_lot" intent
/// without violating the DB CHECK constraint.
#[sqlx::test(migrations = "../../migrations")]
async fn account_too_small_rejects_signal(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "tiny_balance_acct",
        "paper",
        "bitflyer_cfd",
        "boundary_strategy",
        10_000,
    )
    .await;

    let trader = build_trader_with_market(
        pool.clone(),
        account_id,
        Exchange::BitflyerCfd,
        "FX_BTC_JPY",
        dec!(24_999_000), // bid
        dec!(25_000_000), // ask (Long fill price)
        dec!(0.001),
        dec!(0.50),
    )
    .await;

    let signal = make_signal("FX_BTC_JPY", Direction::Long, dec!(0.02), dec!(1.0));
    let result = trader.execute(&signal).await;

    assert!(
        result.is_err(),
        "execute must fail when raw_qty < min_lot, got Ok({:?})",
        result.ok()
    );
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("balance too small") || msg.contains("minimum order"),
        "error must mention insufficient sizing, got: {msg}"
    );

    // Sanity check: the sizer would indeed reject this (raw=0.0008 < 0.001).
    let raw = sizing_invariants::compute_raw_quantity(
        dec!(10_000),
        dec!(2),
        dec!(1.0),
        dec!(0.02),
        dec!(0.50),
        dec!(25_000_000),
    );
    assert_eq!(raw, dec!(0.0008), "raw qty must be below 0.001 min_lot");

    // Sanity: balance unchanged because no fill happened.
    let bal = current_balance(&pool, account_id).await;
    assert_eq!(bal, dec!(10_000), "balance must be untouched on rejected signal");
}

/// **Test 2**: balance=30,000 yen on bitflyer_cfd at BTC = 12.5M with lev=2
/// produces raw_qty = 0.0048 BTC → truncates to 0.004 (multiple of 0.001).
/// Verifies the realistic bitFlyer Crypto CFD case: cap-binding alloc=1.0
/// (not LC-binding) plus min_lot floor.
#[sqlx::test(migrations = "../../migrations")]
async fn min_lot_truncation_realistic_bitflyer(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "btc_truncation_acct",
        "paper",
        "bitflyer_cfd",
        "boundary_strategy",
        30_000,
    )
    .await;

    let trader = build_trader_with_market(
        pool.clone(),
        account_id,
        Exchange::BitflyerCfd,
        "FX_BTC_JPY",
        dec!(12_499_000),
        dec!(12_500_000), // ask (Long fill)
        dec!(0.001),
        dec!(0.50),
    )
    .await;

    let signal = make_signal("FX_BTC_JPY", Direction::Long, dec!(0.02), dec!(1.0));
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    // raw = 30,000 × 2 × 1.0 / 12,500,000 = 0.0048 → floor to 0.004.
    let expected = sizing_invariants::expected_quantity(
        dec!(30_000),
        dec!(2),
        dec!(1.0),
        dec!(0.02),
        dec!(0.50),
        dec!(12_500_000),
        dec!(0.001),
    );
    assert_eq!(expected, dec!(0.004), "sanity check on expected_quantity");
    assert_eq!(
        trade.quantity,
        dec!(0.004),
        "min_lot truncation: 0.0048 raw → 0.004 (multiple of 0.001)"
    );
    assert_eq!(trade.entry_price, dec!(12_500_000), "Long fills at ask");
    assert_eq!(trade.direction, Direction::Long);

    // Post-SL margin level invariant: in the cap-binding branch margin
    // level lies *strictly above* Y, so the assertion must pass.
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        dec!(30_000),
        dec!(0.50),
    );
}

/// **Test 3**: SL=20% on gmo_fx (Y=1.0, lev=10) forces
///   `max_alloc = 1 / (1.0 + 10 × 0.2) = 1 / 3.0 ≈ 0.3333`,
/// so the LC cap (not the alloc=1.0 cap) is binding. Quantity drops to
/// 30,000 × 10 × 0.3333 / 157 ≈ 636.94 → 636 (min_lot=1).
#[sqlx::test(migrations = "../../migrations")]
async fn lc_constraint_binds_at_extreme_sl_gmo_fx(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "extreme_sl_acct",
        "paper",
        "gmo_fx",
        "boundary_strategy",
        30_000,
    )
    .await;
    override_leverage(&pool, account_id, dec!(10)).await;

    let trader = build_trader_with_market(
        pool.clone(),
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        dec!(156),
        dec!(157), // ask (Long fill)
        dec!(1),
        dec!(1.00),
    )
    .await;

    let signal = make_signal("USD_JPY", Direction::Long, dec!(0.20), dec!(1.0));
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    // Verify the LC cap is the one that bites here.
    let max_alloc =
        sizing_invariants::compute_max_alloc(dec!(10), dec!(0.20), dec!(1.00));
    assert!(
        max_alloc < dec!(1.0),
        "test setup invariant: max_alloc must be below alloc=1.0 \
         to confirm LC-binding (got {max_alloc})"
    );

    // Concrete sizing expectation:
    //   max_alloc = 1 / 3.0 → raw = 30000 × 10 / 3 / 157 ≈ 636.94 → floor 636.
    let expected = sizing_invariants::expected_quantity(
        dec!(30_000),
        dec!(10),
        dec!(1.0),
        dec!(0.20),
        dec!(1.00),
        dec!(157),
        dec!(1),
    );
    assert_eq!(expected, dec!(636), "sanity check on expected_quantity");
    assert_eq!(
        trade.quantity,
        dec!(636),
        "LC cap binding: 30k × 10 / 3 / 157 → 636"
    );
    assert_eq!(trade.entry_price, dec!(157), "Long fills at ask");

    // The whole point of the LC-binding branch is that the post-SL margin
    // level lands at (or just above) the broker threshold Y. Assert it
    // explicitly so a future tweak to the formula that silently breaks the
    // safety promise gets caught here.
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        dec!(30_000),
        dec!(1.00),
    );
}

/// **Test 4**: Two consecutive `trader.execute` calls share the same
/// account balance. The first locks margin and decreases `current_balance`;
/// the second must size against the post-margin-lock balance, so its
/// quantity is strictly smaller than the first.
///
/// This bypasses the strategy-level duplicate-signal guard (which lives
/// in the signal executor, not the Trader) and exercises the Trader's
/// reliance on the live `current_balance` row.
#[sqlx::test(migrations = "../../migrations")]
async fn multiple_open_positions_share_balance_correctly(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "shared_balance_acct",
        "paper",
        "gmo_fx",
        "boundary_strategy",
        30_000,
    )
    .await;
    override_leverage(&pool, account_id, dec!(10)).await;

    let trader = build_trader_with_market(
        pool.clone(),
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        dec!(156),
        dec!(157), // ask (Long fill)
        dec!(1),
        dec!(1.00),
    )
    .await;

    // First execute: balance=30,000.
    let signal1 = make_signal("USD_JPY", Direction::Long, dec!(0.02), dec!(1.0));
    let trade1 = trader
        .execute(&signal1)
        .await
        .expect("first execute should succeed");

    // Concrete expectation for trade 1:
    //   max_alloc = 1/(1.0+10×0.02)=1/1.2=0.8333…
    //   raw = 30,000 × 10 × 0.8333 / 157 = 1592.356 → floor 1592.
    let expected_qty1 = sizing_invariants::expected_quantity(
        dec!(30_000),
        dec!(10),
        dec!(1.0),
        dec!(0.02),
        dec!(1.00),
        dec!(157),
        dec!(1),
    );
    assert_eq!(expected_qty1, dec!(1592));
    assert_eq!(trade1.quantity, dec!(1592), "trade 1 qty");

    // Margin lock for trade 1: truncate_yen(1592 × 157 / 10) = truncate_yen(24994.4) = 24994.
    let margin1 = sizing_invariants::expected_margin_lock(
        trade1.quantity,
        trade1.entry_price,
        trade1.leverage,
    );
    assert_eq!(margin1, dec!(24_994), "expected margin lock for trade 1");

    // Balance after first execute must reflect the margin lock.
    let balance_after_1 = current_balance(&pool, account_id).await;
    assert_eq!(
        balance_after_1,
        dec!(30_000) - margin1,
        "balance must reflect margin_lock after first execute"
    );
    assert_eq!(balance_after_1, dec!(5_006));

    // Second execute reuses the same trader/account — must size against
    // the reduced balance, NOT the initial 30,000.
    let signal2 = make_signal("USD_JPY", Direction::Long, dec!(0.02), dec!(1.0));
    let trade2 = trader
        .execute(&signal2)
        .await
        .expect("second execute should succeed");

    // Expected qty for trade 2:
    //   raw = 5006 × 10 × (1/1.2) / 157 = 5006 × 10 / 1.2 / 157
    //        = 50060 / 1.2 / 157 = 41716.6666… / 157 = 265.711… → floor 265.
    let expected_qty2 = sizing_invariants::expected_quantity(
        balance_after_1,
        dec!(10),
        dec!(1.0),
        dec!(0.02),
        dec!(1.00),
        dec!(157),
        dec!(1),
    );
    assert_eq!(expected_qty2, dec!(265), "sanity check on expected_quantity for trade 2");
    assert_eq!(
        trade2.quantity,
        dec!(265),
        "trade 2 must size against post-margin-lock balance"
    );

    // Strict inequality is the core invariant: balance was consumed.
    assert!(
        trade2.quantity < trade1.quantity,
        "trade 2 quantity ({}) must be smaller than trade 1 ({}) — \
         second execute must observe the margin lock",
        trade2.quantity,
        trade1.quantity,
    );

    // Balance after trade 2 must reflect both margin locks.
    // Concrete values pin the boundary scenario for future readers:
    //   margin2 = truncate_yen(265 × 157 / 10) = truncate_yen(4160.5) = 4160
    //   balance_after_2 = 5006 - 4160 = 846
    let margin2 = sizing_invariants::expected_margin_lock(
        trade2.quantity,
        trade2.entry_price,
        trade2.leverage,
    );
    assert_eq!(margin2, dec!(4_160), "expected margin lock for trade 2");
    let balance_after_2 = current_balance(&pool, account_id).await;
    assert_eq!(
        balance_after_2,
        balance_after_1 - margin2,
        "balance after trade 2 must reflect cumulative margin locks"
    );
    assert_eq!(
        balance_after_2,
        dec!(846),
        "free cash drops to 846 yen after both margin locks (30000 → 5006 → 846)"
    );
}
