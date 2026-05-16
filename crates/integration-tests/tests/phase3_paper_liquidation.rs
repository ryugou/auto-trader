//! Phase 3: paper account の維持率ロスカット判定テスト。
//!
//! `detect_liquidation_targets` が paper account について、
//! 維持率 `< threshold` で正しく全 trade_id を返すこと、
//! および live account / price 不在の account を skip することを確認する。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::event::{PriceEvent, TradeAction, TradeEvent};
use auto_trader_core::types::{Candle, Direction, Exchange, ExitReason, Pair, Trade, TradeStatus};
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
async fn liquidation_does_not_fire_at_exact_threshold(pool: sqlx::PgPool) {
    // 境界条件: ratio == threshold は `<` のみ発火の契約により発火しない。
    // この test は `<=` への regression を防ぐ guard (Copilot round-4 指摘)。
    //
    // balance after lock = 40k, required = 60k, threshold = 1.00。
    // ratio = 1.00 となるには equity = required = 60k:
    //   equity = current_balance + lock 戻し(60k) + unrealized = 60k
    //   → current_balance + unrealized = 0
    //   → 40k + unrealized = 0 → unrealized = -40k
    //   → (current - 150) * 10000 = -40000 → current = 146
    let account_id = seed_trading_account(
        &pool,
        "liq_boundary",
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

    let ps = make_price_store(Exchange::GmoFx, "USD_JPY", dec!(146), dec!(146.1)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(146));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("liq_boundary".into()),
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
        "ratio == threshold (1.00) must NOT liquidate; \
         only strictly `< threshold` fires (regression guard for `<=`)"
    );
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

#[sqlx::test(migrations = "../../migrations")]
async fn liquidation_returns_all_trades_in_breached_account(pool: sqlx::PgPool) {
    // 同 account に 2 open trade (USD_JPY + EUR_USD) を持ち、両 pair が大きく
    // 逆行して維持率が threshold を下回ったとき、戻り値が両 trade_id を含む
    // ことを確認する。account 単位 close の new contract を guard する test。
    let account_id = seed_trading_account(
        &pool,
        "liq_multi",
        "paper",
        "gmo_fx",
        "test_strategy",
        100_000,
    )
    .await;

    // trade1: USD_JPY Long, entry=150, qty=5000, lev=25 → required=30k
    let trade1 = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(5000),
        dec!(25),
    );
    seed_and_lock(&pool, &trade1).await;

    // trade2: EUR_USD Long, entry=1.10, qty=27272.728 (≈ price=1.1, qty で required≈1200)
    // ただし price_unit 等の精度確認は本テストではなく、ここでは別 pair に open trade
    // が同 account にいるという事実だけを表現したい。entry/qty を小さく取り required は
    // trade1 主体に。required(trade2) = 1.1 * 1000 / 25 = 44 (微小)
    let trade2 = make_trade(
        account_id,
        Exchange::GmoFx,
        "EUR_USD",
        Direction::Long,
        dec!(1.10),
        dec!(1000),
        dec!(25),
    );
    seed_and_lock(&pool, &trade2).await;

    // balance after both locks ≈ 100000 - 30000 - 44 = 69956
    // 大きな逆行: USD_JPY current=120 → unrealized = (120-150)*5000 = -150000
    //              EUR_USD current=1.10 → unrealized = 0
    // required_total = 30044, unrealized_total = -150000
    // equity = 69956 + 30044 - 150000 = -50000 → ratio < 0 < 1.0 → fire
    let feed_key1 = FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY"));
    let feed_key2 = FeedKey::new(Exchange::GmoFx, Pair::new("EUR_USD"));
    let ps = PriceStore::new(vec![feed_key1.clone(), feed_key2.clone()]);
    ps.update(
        feed_key1,
        LatestTick {
            price: dec!(120),
            best_bid: Some(dec!(120)),
            best_ask: Some(dec!(120.1)),
            ts: Utc::now(),
        },
    )
    .await;
    ps.update(
        feed_key2,
        LatestTick {
            price: dec!(1.10),
            best_bid: Some(dec!(1.10)),
            best_ask: Some(dec!(1.1001)),
            ts: Utc::now(),
        },
    )
    .await;

    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(120));
    let owned1 = OpenTradeWithAccount {
        trade: trade1.clone(),
        account_name: Some("liq_multi".into()),
        account_type: Some("paper".into()),
    };
    let owned2 = OpenTradeWithAccount {
        trade: trade2.clone(),
        account_name: Some("liq_multi".into()),
        account_type: Some("paper".into()),
    };
    let ctx = auto_trader::liquidation::LiquidationContext {
        pool: pool.clone(),
        price_store: ps,
        exchange_liquidation_levels: std::sync::Arc::new(levels()),
        live_forces_dry_run: false,
    };
    let targets =
        auto_trader::liquidation::detect_liquidation_targets(&ctx, &[owned1, owned2], &event).await;
    assert_eq!(targets.len(), 1, "one breached account");
    assert_eq!(targets[0].0, account_id);
    let mut returned_ids = targets[0].1.clone();
    returned_ids.sort();
    let mut expected_ids = vec![trade1.id, trade2.id];
    expected_ids.sort();
    assert_eq!(
        returned_ids, expected_ids,
        "breached account must return ALL its open trades for force-close"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn close_trade_with_liquidation_reason_persists_and_emits_event(pool: sqlx::PgPool) {
    // detect_liquidation_targets で trade_id を取った後、closer::close_trade
    // を呼んで実際に DB の trade 行が `status='closed'` / `exit_reason='liquidation'`
    // になり、`TradeEvent::Closed` が trade_tx に流れることを確認する
    // (Copilot round-3 end-to-end test 不足の指摘)。
    let account_id = seed_trading_account(
        &pool,
        "liq_e2e",
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
    let trade_id = trade.id;

    // close 経路は fill_close (dry_run=true) で PriceStore から bid を読む。
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY", dec!(145), dec!(145.1)).await;

    let mut apis: std::collections::HashMap<
        Exchange,
        Arc<dyn auto_trader_market::exchange_api::ExchangeApi>,
    > = std::collections::HashMap::new();
    apis.insert(
        Exchange::GmoFx,
        Arc::new(auto_trader_market::null_exchange_api::NullExchangeApi),
    );

    let mut min_sizes: HashMap<Pair, Decimal> = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let position_sizer = Arc::new(auto_trader_executor::position_sizer::PositionSizer::new(
        min_sizes,
    ));
    let notifier = Arc::new(auto_trader_notify::Notifier::new_disabled());

    let (trade_tx, mut trade_rx) = tokio::sync::mpsc::channel::<TradeEvent>(16);

    let ctx = auto_trader::closer::CloseContext {
        pool: pool.clone(),
        apis: Arc::new(apis),
        price_store: ps,
        notifier,
        position_sizer,
        liquidation_levels: Arc::new(levels()),
        trade_tx,
    };

    auto_trader::closer::close_trade(
        &ctx,
        &trade,
        "liq_e2e".to_string(),
        "paper".to_string(),
        true,
        ExitReason::Liquidation,
        dec!(145),
    )
    .await;

    // DB row が closed + exit_reason='liquidation' になっていることを確認
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT status, exit_reason FROM trades WHERE id = $1")
            .bind(trade_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0, "closed", "trade status should be closed");
    assert_eq!(
        row.1.as_deref(),
        Some("liquidation"),
        "trade exit_reason should be liquidation"
    );

    // TradeEvent::Closed が emit されていることを確認
    let ev = trade_rx.recv().await.expect("TradeEvent should be emitted");
    assert_eq!(ev.trade.id, trade_id);
    match ev.action {
        TradeAction::Closed { exit_reason, .. } => {
            assert_eq!(exit_reason, ExitReason::Liquidation);
        }
        other => panic!("expected Closed action, got {:?}", other),
    }
}
