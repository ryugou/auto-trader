//! Phase 5: live 残高ドリフト検知の統合テスト。
//!
//! `balance_drift::check_live_accounts` を、live 口座 + open trade + 既知の
//! PriceStore 価格で seed し、MockExchangeApi が返す Collateral を振って
//! 次を検証する:
//!
//! - (a) 取引所 equity が bot equity と一致 → アラート無し
//! - (b) 閾値を超えて乖離 → アラート 1 件
//!
//! deterministic な bot_equity:
//!
//! - current_balance = 1,000,000
//! - open trade: Long USD_JPY entry=150 qty=1000 leverage=2
//! - PriceStore bid=151 → current_price=151 (Long は bid で決済)
//! - required_margin = truncate_yen(150*1000/2) = 75,000
//! - unrealized_pnl  = (151-150)*1000           = 1,000
//! - bot_equity = 1,000,000 + 75,000 + 1,000    = 1,076,000

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader::balance_drift::{BalanceDriftContext, check_live_accounts};
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::helpers::seed::seed_open_trade;
use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApiBuilder;
use auto_trader_market::bitflyer_private::Collateral;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use chrono::Utc;
use rust_decimal_macros::dec;

/// bot_equity を deterministic に組む共通 seed。live gmo_fx 口座 +
/// Long USD_JPY open trade + PriceStore bid/ask を用意して ctx を返す。
async fn setup(pool: sqlx::PgPool, collateral: Collateral) -> (BalanceDriftContext, uuid::Uuid) {
    let account_id = seed_trading_account(
        &pool,
        "phase5_live",
        "live",
        "gmo_fx",
        "donchian_trend_v1",
        1_000_000,
    )
    .await;

    seed_open_trade(
        &pool,
        account_id,
        "donchian_trend_v1",
        "USD_JPY",
        "gmo_fx",
        "long",
        dec!(150),  // entry_price
        dec!(148),  // stop_loss
        dec!(1000), // quantity
        Utc::now(),
    )
    .await;

    // PriceStore: gmo_fx USD_JPY bid=151, ask=151.02 → Long は bid=151。
    let feed_key = FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY"));
    let price_store = PriceStore::new(vec![feed_key.clone()]);
    price_store
        .update(
            feed_key,
            LatestTick {
                price: dec!(151.01),
                best_bid: Some(dec!(151)),
                best_ask: Some(dec!(151.02)),
                ts: Utc::now(),
            },
        )
        .await;

    let mock = MockExchangeApiBuilder::new()
        .with_get_collateral_response(collateral)
        .build();
    let mut apis: HashMap<Exchange, Arc<dyn ExchangeApi>> = HashMap::new();
    apis.insert(Exchange::GmoFx, mock);

    let ctx = BalanceDriftContext {
        pool,
        price_store,
        apis: Arc::new(apis),
        live_forces_dry_run: false,
    };
    (ctx, account_id)
}

#[sqlx::test(migrations = "../../migrations")]
async fn no_alert_when_exchange_equity_matches_bot_equity(pool: sqlx::PgPool) {
    // exchange_equity = 1,075,000 + 1,000 = 1,076,000 == bot_equity → drift 無し。
    let collateral = Collateral {
        collateral: dec!(1075000),
        open_position_pnl: dec!(1000),
        require_collateral: dec!(75000),
        keep_rate: dec!(10),
    };
    let (ctx, _id) = setup(pool, collateral).await;
    let alerts = check_live_accounts(&ctx).await;
    assert!(
        alerts.is_empty(),
        "matching equity must not alert, got: {alerts:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn one_alert_when_exchange_equity_diverges_beyond_threshold(pool: sqlx::PgPool) {
    // exchange_equity = 1,100,000 + 1,000 = 1,101,000.
    // bot_equity = 1,076,000. diff = 25,000 > threshold(max(1% of 1,101,000, 500)=11,010) → alert。
    let collateral = Collateral {
        collateral: dec!(1100000),
        open_position_pnl: dec!(1000),
        require_collateral: dec!(75000),
        keep_rate: dec!(12),
    };
    let (ctx, _id) = setup(pool, collateral).await;
    let alerts = check_live_accounts(&ctx).await;
    assert_eq!(alerts.len(), 1, "diverging equity must alert once");
    let ev = &alerts[0];
    assert_eq!(ev.title, "balance drift");
    assert_eq!(ev.account_name, "phase5_live");
    assert_eq!(ev.exchange, Exchange::GmoFx);
    assert!(
        ev.body.contains("exchange equity=1101000") && ev.body.contains("bot equity=1076000"),
        "body should carry both equities: {}",
        ev.body
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn skips_all_when_live_forces_dry_run(pool: sqlx::PgPool) {
    // LIVE_DRY_RUN 強制時は取引所残高が動かない → 判定 skip (乖離があっても空)。
    let collateral = Collateral {
        collateral: dec!(1100000),
        open_position_pnl: dec!(1000),
        require_collateral: dec!(75000),
        keep_rate: dec!(12),
    };
    let (mut ctx, _id) = setup(pool, collateral).await;
    ctx.live_forces_dry_run = true;
    let alerts = check_live_accounts(&ctx).await;
    assert!(
        alerts.is_empty(),
        "live_forces_dry_run must skip drift check"
    );
}
