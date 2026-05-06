//! Phase 3: Signal-routing e2e tests (Plan Task 5).
//!
//! These tests drive the multi-account signal routing contract end-to-end:
//! given a signal and a set of accounts (each with its own
//! `(exchange, allowed_pairs, Trader)` binding), only the accounts whose
//! exchange supports the signal's pair should produce a trade.
//!
//! The routing rule is reproduced inline below (mirroring the
//! `executor_exchange_pairs` filter in `crates/app/src/main.rs`'s signal
//! executor task). This guarantees that the dispatch contract — "a signal
//! lands on the trader whose exchange owns its pair, and nowhere else" —
//! is exercised against real `Trader::execute` calls hitting the DB.

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, Pair, Signal, Trade};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_market::null_exchange_api::NullExchangeApi;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use uuid::Uuid;

const BTC_PAIR: &str = "FX_BTC_JPY";
const USD_PAIR: &str = "USD_JPY";

/// One account in the routing fan-out: the trader plus the set of pairs
/// its exchange is configured to handle (mirrors `executor_exchange_pairs`
/// in `main.rs`).
struct RoutedAccount {
    name: String,
    account_id: Uuid,
    trader: Trader,
    allowed_pairs: Vec<Pair>,
}

/// Reproduce the pair-filter routing from `main.rs`:
///
/// ```ignore
/// if !executor_exchange_pairs.get(&exchange).is_some_and(|pairs| pairs.contains(&signal.pair.0)) {
///     continue;  // skip — this account's exchange does not own the signal's pair
/// }
/// trader.execute(signal).await?
/// ```
///
/// Returns the trades that were actually opened, paired with the
/// originating account name so callers can assert which side fired.
async fn route_and_execute(signal: &Signal, accounts: &[RoutedAccount]) -> Vec<(String, Trade)> {
    let mut trades = Vec::new();
    for acc in accounts {
        if !acc.allowed_pairs.contains(&signal.pair) {
            continue;
        }
        match acc.trader.execute(signal).await {
            Ok(trade) => trades.push((acc.name.clone(), trade)),
            Err(e) => panic!("trader.execute for {} unexpectedly failed: {e}", acc.name),
        }
    }
    trades
}

async fn seed_price(price_store: &PriceStore, exchange: Exchange, pair: &Pair, mid: Decimal) {
    let feed_key = FeedKey::new(exchange, pair.clone());
    price_store
        .update(
            feed_key,
            LatestTick {
                price: mid,
                best_bid: Some(mid - dec!(0.5)),
                best_ask: Some(mid + dec!(0.5)),
                ts: Utc::now(),
            },
        )
        .await;
}

/// Build the standard 2-account fan-out:
/// - bitflyer_cfd / FX_BTC_JPY / bb_mean_revert_v1
/// - gmo_fx       / USD_JPY    / donchian_trend_v1
///
/// Both share a single `PriceStore` carrying ticks for both feeds, exactly
/// as the production executor task does.
async fn build_two_account_fanout(pool: PgPool) -> Vec<RoutedAccount> {
    let bitflyer_pair = Pair::new(BTC_PAIR);
    let gmo_pair = Pair::new(USD_PAIR);

    let price_store = PriceStore::new(vec![
        FeedKey::new(Exchange::BitflyerCfd, bitflyer_pair.clone()),
        FeedKey::new(Exchange::GmoFx, gmo_pair.clone()),
    ]);

    // Realistic mids so sizing produces a positive quantity post-truncation.
    seed_price(&price_store, Exchange::BitflyerCfd, &bitflyer_pair, dec!(15_000_000)).await;
    seed_price(&price_store, Exchange::GmoFx, &gmo_pair, dec!(150)).await;

    let mut min_sizes: HashMap<Pair, Decimal> = HashMap::new();
    min_sizes.insert(bitflyer_pair.clone(), dec!(0.001));
    min_sizes.insert(gmo_pair.clone(), dec!(1));
    let sizer = Arc::new(PositionSizer::new(min_sizes));
    let notifier = Arc::new(Notifier::new_disabled());

    let bitflyer_account_id = seed_trading_account(
        &pool,
        "routing_bitflyer",
        "paper",
        "bitflyer_cfd",
        "bb_mean_revert_v1",
        30_000,
    )
    .await;

    let gmo_account_id = seed_trading_account(
        &pool,
        "routing_gmo",
        "paper",
        "gmo_fx",
        "donchian_trend_v1",
        30_000,
    )
    .await;

    let api: Arc<dyn auto_trader_market::exchange_api::ExchangeApi> = Arc::new(NullExchangeApi);

    let bitflyer_trader = Trader::new(
        pool.clone(),
        Exchange::BitflyerCfd,
        bitflyer_account_id,
        "routing_bitflyer".to_string(),
        api.clone(),
        price_store.clone(),
        notifier.clone(),
        sizer.clone(),
        dec!(0.50), // bitFlyer Crypto CFD 維持率 50%
        true,       // dry_run
    );

    let gmo_trader = Trader::new(
        pool.clone(),
        Exchange::GmoFx,
        gmo_account_id,
        "routing_gmo".to_string(),
        api,
        price_store.clone(),
        notifier,
        sizer,
        dec!(1.00), // GMO 外為 FX 維持率 100%
        true,
    );

    vec![
        RoutedAccount {
            name: "routing_bitflyer".to_string(),
            account_id: bitflyer_account_id,
            trader: bitflyer_trader,
            allowed_pairs: vec![bitflyer_pair],
        },
        RoutedAccount {
            name: "routing_gmo".to_string(),
            account_id: gmo_account_id,
            trader: gmo_trader,
            allowed_pairs: vec![gmo_pair],
        },
    ]
}

fn make_signal(pair: &str, direction: Direction, strategy_name: &str) -> Signal {
    Signal {
        strategy_name: strategy_name.to_string(),
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

async fn count_trades_for_account(pool: &PgPool, account_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM trades WHERE account_id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("count trades")
}

// ─── 1. FX_BTC_JPY signal routes only to BitflyerCfd ─────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn signal_for_btc_routes_to_bitflyer_only(pool: PgPool) {
    let accounts = build_two_account_fanout(pool.clone()).await;
    let bitflyer_id = accounts[0].account_id;
    let gmo_id = accounts[1].account_id;

    let signal = make_signal(BTC_PAIR, Direction::Long, "bb_mean_revert_v1");
    let trades = route_and_execute(&signal, &accounts).await;

    assert_eq!(
        trades.len(),
        1,
        "exactly one account should fire for FX_BTC_JPY signal, got {trades:?}"
    );
    assert_eq!(trades[0].0, "routing_bitflyer", "BTC signal must land on bitflyer_cfd");
    assert_eq!(trades[0].1.exchange, Exchange::BitflyerCfd);
    assert_eq!(trades[0].1.pair, Pair::new(BTC_PAIR));
    assert_eq!(trades[0].1.account_id, bitflyer_id);

    assert_eq!(
        count_trades_for_account(&pool, bitflyer_id).await,
        1,
        "trades table must show exactly 1 trade for bitflyer account"
    );
    assert_eq!(
        count_trades_for_account(&pool, gmo_id).await,
        0,
        "gmo_fx account must have zero trades — pair filter must reject"
    );
}

// ─── 2. USD_JPY signal routes only to GmoFx ──────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn signal_for_usdjpy_routes_to_gmofx_only(pool: PgPool) {
    let accounts = build_two_account_fanout(pool.clone()).await;
    let bitflyer_id = accounts[0].account_id;
    let gmo_id = accounts[1].account_id;

    let signal = make_signal(USD_PAIR, Direction::Long, "donchian_trend_v1");
    let trades = route_and_execute(&signal, &accounts).await;

    assert_eq!(
        trades.len(),
        1,
        "exactly one account should fire for USD_JPY signal, got {trades:?}"
    );
    assert_eq!(trades[0].0, "routing_gmo", "USD_JPY signal must land on gmo_fx");
    assert_eq!(trades[0].1.exchange, Exchange::GmoFx);
    assert_eq!(trades[0].1.pair, Pair::new(USD_PAIR));
    assert_eq!(trades[0].1.account_id, gmo_id);

    assert_eq!(
        count_trades_for_account(&pool, gmo_id).await,
        1,
        "trades table must show exactly 1 trade for gmo_fx account"
    );
    assert_eq!(
        count_trades_for_account(&pool, bitflyer_id).await,
        0,
        "bitflyer account must have zero trades — pair filter must reject"
    );
}

// ─── 3. Unknown pair routes nowhere ──────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_pair_routes_nowhere(pool: PgPool) {
    let accounts = build_two_account_fanout(pool.clone()).await;
    let bitflyer_id = accounts[0].account_id;
    let gmo_id = accounts[1].account_id;

    // EUR_USD is not in the allowed_pairs of either account, so the routing
    // filter must reject it on both sides without ever calling execute.
    let signal = make_signal("EUR_USD", Direction::Long, "bb_mean_revert_v1");
    let trades = route_and_execute(&signal, &accounts).await;

    assert!(
        trades.is_empty(),
        "unknown-pair signal must produce zero trades, got {trades:?}"
    );
    assert_eq!(
        count_trades_for_account(&pool, bitflyer_id).await,
        0,
        "bitflyer account must have zero trades for unknown pair"
    );
    assert_eq!(
        count_trades_for_account(&pool, gmo_id).await,
        0,
        "gmo_fx account must have zero trades for unknown pair"
    );

    // Belt-and-suspenders: the global trades table must also be empty.
    let total_trades: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trades")
        .fetch_one(&pool)
        .await
        .expect("count trades");
    assert_eq!(
        total_trades, 0,
        "no trade row must be created for an unrouted signal"
    );
}
