//! Phase 3 liquidation-safety flow tests — verify the core promise of the
//! broker-aware sizing formula `max_alloc = 1 / (Y + leverage × stop_loss_pct)`:
//! at the moment the stop-loss hits, the post-fill margin level lands at
//! exactly the broker liquidation threshold `Y` (or higher, when the alloc
//! cap binds before the LC cap).
//!
//! These tests exercise the same minimal `Trader` setup as
//! `phase3_sizing_boundaries.rs` but focus on margin-level invariants —
//! both at the SL price (Tests 1, 2, 5, 6) and along the path leading up
//! to it (Test 3) and beyond it (Test 4).

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
        strategy_name: "liquidation_safety_strategy".to_string(),
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
/// gmo_fx tests can run with the production-realistic 10x leverage.
async fn override_leverage(pool: &sqlx::PgPool, account_id: Uuid, leverage: Decimal) {
    sqlx::query("UPDATE trading_accounts SET leverage = $1 WHERE id = $2")
        .bind(leverage)
        .bind(account_id)
        .execute(pool)
        .await
        .expect("leverage update should succeed");
}

/// Read the current_balance for "balance at open" assertions.
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
///
/// Mirrors `phase3_sizing_boundaries.rs::build_trader_with_market` —
/// kept locally to avoid widening `helpers::sizing_invariants`'s public
/// surface in this PR. Future Task 7 may unify them.
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
        "liquidation_safety_account".to_string(),
        Arc::new(NullExchangeApi),
        price_store,
        Arc::new(Notifier::new_disabled()),
        sizer,
        liquidation_margin_level,
        true, // dry_run
    )
}

/// Compute `equity_at_price / margin_used` for an open Long trade,
/// given the spot price after some drawdown. Used by the path-dependent
/// margin level checks (Tests 3 and 4).
fn margin_level_at_price(
    balance_at_open: Decimal,
    quantity: Decimal,
    entry_price: Decimal,
    leverage: Decimal,
    spot_price: Decimal,
) -> Decimal {
    let pnl = (spot_price - entry_price) * quantity;
    let equity = balance_at_open + pnl;
    let margin_used = quantity * entry_price / leverage;
    equity / margin_used
}

// ─── Tests ─────────────────────────────────────────────────────────────────

/// **Test 1**: bitflyer_cfd, balance=30k, lev=2, FX_BTC_JPY @ 12.5M,
/// alloc=1.0, SL=2%, Y=0.5.
///
/// `max_alloc = 1/(0.5 + 2×0.02) = 1/0.54 ≈ 1.85` → cap binds at alloc=1.0.
/// In the cap-binding branch the post-SL margin level lies *strictly above*
/// Y because the position is smaller than the LC formula would allow.
///
/// Concrete: qty = 0.004 (raw 0.0048 floored to 0.001 lot),
/// pnl_at_sl = 0.004 × 12_500_000 × 0.02 = -1000 yen,
/// margin_used = 0.004 × 12_500_000 / 2 = 25000,
/// margin_level = (30000 - 1000)/25000 = 1.16, well above Y=0.5.
#[sqlx::test(migrations = "../../migrations")]
async fn post_sl_margin_level_above_y_bitflyer_cfd_cap_binding(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "lc_safety_bitflyer",
        "paper",
        "bitflyer_cfd",
        "liquidation_safety_strategy",
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
    let balance_before = current_balance(&pool, account_id).await;
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    // Sanity: cap-binding branch produced qty=0.004.
    assert_eq!(trade.quantity, dec!(0.004));

    // Core invariant.
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before,
        dec!(0.50),
    );

    // Pin the concrete margin level so a future regression that erodes
    // the cap-binding cushion is caught.
    let pnl_at_sl = (trade.stop_loss - trade.entry_price) * trade.quantity;
    let equity_at_sl = balance_before + pnl_at_sl;
    let margin_used = trade.quantity * trade.entry_price / trade.leverage;
    let margin_level = equity_at_sl / margin_used;
    assert_eq!(pnl_at_sl, dec!(-1000), "pnl_at_sl: -250000 × 0.004");
    assert_eq!(margin_used, dec!(25000), "margin_used = qty × entry / lev");
    assert_eq!(margin_level, dec!(1.16), "(30000 - 1000)/25000");
    assert!(
        margin_level > dec!(0.50),
        "cap-binding branch must give cushion above Y={}, got {}",
        dec!(0.50),
        margin_level
    );
}

/// **Test 2**: gmo_fx, balance=30k, lev=10, USD_JPY @ 157, alloc=1.0,
/// SL=2%, Y=1.0.
///
/// `max_alloc = 1/(1.0 + 10×0.02) = 1/1.2 ≈ 0.8333` → LC cap binds.
/// At exact-cap qty the post-SL margin level equals Y; min_lot truncation
/// pushes it just barely above Y.
///
/// Concrete: qty = 1592 (raw 1592.36 floored), pnl_at_sl = -4998.88,
/// margin_used = 24994.4, margin_level ≈ 1.000269.
#[sqlx::test(migrations = "../../migrations")]
async fn post_sl_margin_level_at_y_gmo_fx(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "lc_safety_gmo",
        "paper",
        "gmo_fx",
        "liquidation_safety_strategy",
        30_000,
    )
    .await;
    override_leverage(&pool, account_id, dec!(10)).await;

    let trader = build_trader_with_market(
        pool.clone(),
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        dec!(156.99),
        dec!(157.00),
        dec!(1),
        dec!(1.00),
    )
    .await;

    let signal = make_signal("USD_JPY", Direction::Long, dec!(0.02), dec!(1.0));
    let balance_before = current_balance(&pool, account_id).await;
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    // Sanity: LC-binding branch produced qty=1592.
    assert_eq!(trade.quantity, dec!(1592));

    // Core invariant.
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before,
        dec!(1.00),
    );

    // Pin the post-SL margin level to exactly Y (within the 0.5% tolerance
    // dictated by min_lot truncation). At exact-cap qty=1592.36 the level
    // would be 1.0; truncating to 1592 nudges it to ≈ 1.000269.
    let pnl_at_sl = (trade.stop_loss - trade.entry_price) * trade.quantity;
    let equity_at_sl = balance_before + pnl_at_sl;
    let margin_used = trade.quantity * trade.entry_price / trade.leverage;
    let margin_level = equity_at_sl / margin_used;

    let target = dec!(1.00);
    let diff = (margin_level - target).abs();
    assert!(
        diff < dec!(0.005),
        "margin level should be exactly {target} (within 0.5%), got {margin_level} \
         (pnl_at_sl={pnl_at_sl}, equity={equity_at_sl}, margin_used={margin_used})"
    );
    assert!(
        margin_level >= target - dec!(0.001),
        "margin level must not breach Y, got {margin_level}"
    );
}

/// **Test 3**: gmo_fx, balance=30k, lev=10, USD_JPY @ 157, alloc=1.0,
/// SL=1%, Y=1.0. Walk the price path from entry toward SL and assert the
/// margin level stays at or above Y all the way to the stop.
///
/// `max_alloc = 1/(1.0 + 10×0.01) = 1/1.1 ≈ 0.909`, qty = 1737
/// (raw 1737.116 floored). At entry the cushion is large; it shrinks
/// toward Y as price approaches SL but never falls below Y until the
/// stop hits.
#[sqlx::test(migrations = "../../migrations")]
async fn pre_sl_drawdown_stays_above_y_gmo_fx(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "lc_safety_pre_sl",
        "paper",
        "gmo_fx",
        "liquidation_safety_strategy",
        30_000,
    )
    .await;
    override_leverage(&pool, account_id, dec!(10)).await;

    let trader = build_trader_with_market(
        pool.clone(),
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        dec!(156.99),
        dec!(157.00),
        dec!(1),
        dec!(1.00),
    )
    .await;

    let signal = make_signal("USD_JPY", Direction::Long, dec!(0.01), dec!(1.0));
    let balance_before = current_balance(&pool, account_id).await;
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    // Sanity: SL=1% should land us at qty=1737.
    assert_eq!(trade.quantity, dec!(1737));
    assert_eq!(trade.entry_price, dec!(157));
    assert_eq!(trade.stop_loss, dec!(155.43));

    let drawdown_to_sl = trade.entry_price - trade.stop_loss; // 1.57
    assert_eq!(drawdown_to_sl, dec!(1.57));
    let y = dec!(1.00);

    // 50% of the way to SL.
    let p_50 = trade.entry_price - drawdown_to_sl * dec!(0.5);
    let lvl_50 = margin_level_at_price(
        balance_before,
        trade.quantity,
        trade.entry_price,
        trade.leverage,
        p_50,
    );
    assert!(
        lvl_50 >= y,
        "at 50% drawdown margin level {lvl_50} must be >= Y={y}"
    );

    // 80% of the way.
    let p_80 = trade.entry_price - drawdown_to_sl * dec!(0.8);
    let lvl_80 = margin_level_at_price(
        balance_before,
        trade.quantity,
        trade.entry_price,
        trade.leverage,
        p_80,
    );
    assert!(
        lvl_80 >= y,
        "at 80% drawdown margin level {lvl_80} must be >= Y={y}"
    );

    // 99% of the way — closest to SL while still pre-SL.
    let p_99 = trade.entry_price - drawdown_to_sl * dec!(0.99);
    let lvl_99 = margin_level_at_price(
        balance_before,
        trade.quantity,
        trade.entry_price,
        trade.leverage,
        p_99,
    );
    assert!(
        lvl_99 >= y,
        "at 99% drawdown margin level {lvl_99} must be >= Y={y}"
    );

    // Strict monotonicity: the cushion shrinks as price approaches SL.
    assert!(
        lvl_50 > lvl_80,
        "margin level must decrease as drawdown grows ({lvl_50} -> {lvl_80})"
    );
    assert!(
        lvl_80 > lvl_99,
        "margin level must decrease as drawdown grows ({lvl_80} -> {lvl_99})"
    );

    // The standing invariant at the SL itself.
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before,
        y,
    );
}

/// **Test 4** (negative case — gap-through): same setup as Test 3, but
/// price overshoots SL. The post-fill margin level is *expected* to fall
/// below Y, since the broker formula only protects up to the SL price —
/// gaps past the stop are spec-correct liquidation territory.
#[sqlx::test(migrations = "../../migrations")]
async fn gap_through_sl_breaches_y_gmo_fx(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "lc_safety_gap",
        "paper",
        "gmo_fx",
        "liquidation_safety_strategy",
        30_000,
    )
    .await;
    override_leverage(&pool, account_id, dec!(10)).await;

    let trader = build_trader_with_market(
        pool.clone(),
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        dec!(156.99),
        dec!(157.00),
        dec!(1),
        dec!(1.00),
    )
    .await;

    let signal = make_signal("USD_JPY", Direction::Long, dec!(0.01), dec!(1.0));
    let balance_before = current_balance(&pool, account_id).await;
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    // Sanity: matches Test 3's setup numerically.
    assert_eq!(trade.quantity, dec!(1737));
    let drawdown_to_sl = trade.entry_price - trade.stop_loss;
    assert_eq!(drawdown_to_sl, dec!(1.57));
    let y = dec!(1.00);

    // 110% gap — just past SL.
    let p_110 = trade.entry_price - drawdown_to_sl * dec!(1.10);
    let lvl_110 = margin_level_at_price(
        balance_before,
        trade.quantity,
        trade.entry_price,
        trade.leverage,
        p_110,
    );
    assert!(
        lvl_110 < y,
        "at 110% gap margin level {lvl_110} must be < Y={y} (spec: gap territory)"
    );

    // 150% gap — well past SL.
    let p_150 = trade.entry_price - drawdown_to_sl * dec!(1.50);
    let lvl_150 = margin_level_at_price(
        balance_before,
        trade.quantity,
        trade.entry_price,
        trade.leverage,
        p_150,
    );
    assert!(
        lvl_150 < y,
        "at 150% gap margin level {lvl_150} must be < Y={y}"
    );
    assert!(
        lvl_150 < lvl_110,
        "deeper gap must yield lower margin level ({lvl_110} -> {lvl_150})"
    );
}

/// **Test 5**: gmo_fx, balance=30k, lev=10, SL=0.5% (very tight), Y=1.0.
///
/// `max_alloc = 1/(1.0 + 10×0.005) = 1/1.05 ≈ 0.9524` — LC cap binds.
/// Verifies the formula still holds at the *tight-SL* edge of realistic
/// trading: qty=1819 (raw 1819.84 floored), margin level ≈ 1.000483
/// at SL.
#[sqlx::test(migrations = "../../migrations")]
async fn post_sl_margin_level_at_y_gmo_fx_with_tight_sl(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "lc_safety_tight",
        "paper",
        "gmo_fx",
        "liquidation_safety_strategy",
        30_000,
    )
    .await;
    override_leverage(&pool, account_id, dec!(10)).await;

    let trader = build_trader_with_market(
        pool.clone(),
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        dec!(156.99),
        dec!(157.00),
        dec!(1),
        dec!(1.00),
    )
    .await;

    let signal = make_signal("USD_JPY", Direction::Long, dec!(0.005), dec!(1.0));
    let balance_before = current_balance(&pool, account_id).await;
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    // Sanity: tight-SL LC-binding qty.
    assert_eq!(trade.quantity, dec!(1819));

    // Core invariant.
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before,
        dec!(1.00),
    );

    // Pin the post-SL margin level to exactly Y within tolerance.
    let pnl_at_sl = (trade.stop_loss - trade.entry_price) * trade.quantity;
    let equity_at_sl = balance_before + pnl_at_sl;
    let margin_used = trade.quantity * trade.entry_price / trade.leverage;
    let margin_level = equity_at_sl / margin_used;

    let target = dec!(1.00);
    let diff = (margin_level - target).abs();
    assert!(
        diff < dec!(0.005),
        "margin level should be ≈ {target} (within 0.5%), got {margin_level} \
         (pnl_at_sl={pnl_at_sl}, equity={equity_at_sl}, margin_used={margin_used})"
    );
}

/// **Test 6**: bitflyer_cfd, balance=30k, lev=2, SL=3% (the BB strategy
/// SL_CAP), Y=0.5.
///
/// `max_alloc = 1/(0.5 + 2×0.03) = 1/0.56 ≈ 1.79` → cap binds at
/// alloc=1.0. With lev=2 the post-SL margin level always sits well above
/// Y in realistic JPY-denominated bitflyer cases — the cap, not the LC,
/// is the binding constraint.
#[sqlx::test(migrations = "../../migrations")]
async fn bitflyer_cfd_max_alloc_capped_at_one_realistic(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "lc_safety_bitflyer_cap",
        "paper",
        "bitflyer_cfd",
        "liquidation_safety_strategy",
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

    let signal = make_signal("FX_BTC_JPY", Direction::Long, dec!(0.03), dec!(1.0));
    let balance_before = current_balance(&pool, account_id).await;
    let trade = trader
        .execute(&signal)
        .await
        .expect("execute should succeed");

    // Sanity: same qty=0.004 as Test 1 — alloc=1.0 cap drives it,
    // not the SL%.
    assert_eq!(trade.quantity, dec!(0.004));

    // Verify the cap (not LC) is binding.
    let max_alloc =
        sizing_invariants::compute_max_alloc(dec!(2), dec!(0.03), dec!(0.50));
    assert!(
        max_alloc > dec!(1.0),
        "test setup invariant: max_alloc must exceed alloc=1.0 to confirm \
         cap-binding (got {max_alloc})"
    );

    // Core invariant.
    sizing_invariants::assert_post_sl_margin_level_at_least_y(
        &trade,
        balance_before,
        dec!(0.50),
    );

    // Pin the cushion explicitly:
    //   pnl_at_sl = (12_125_000 - 12_500_000) × 0.004 = -1500
    //   margin_used = 0.004 × 12_500_000 / 2 = 25_000
    //   margin_level = (30_000 - 1500)/25_000 = 1.14
    let pnl_at_sl = (trade.stop_loss - trade.entry_price) * trade.quantity;
    let equity_at_sl = balance_before + pnl_at_sl;
    let margin_used = trade.quantity * trade.entry_price / trade.leverage;
    let margin_level = equity_at_sl / margin_used;
    assert_eq!(pnl_at_sl, dec!(-1500));
    assert_eq!(margin_used, dec!(25000));
    assert_eq!(margin_level, dec!(1.14));
    assert!(
        margin_level > dec!(0.50) + dec!(0.5),
        "with lev=2 and 3% SL the cap-binding cushion must exceed Y by a wide margin, \
         got {margin_level}"
    );
}
