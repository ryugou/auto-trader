//! Phase 3: paper bitFlyer SFD accrual の DB レベル統合テスト。
//!
//! `compute_hourly_sfd` で fee を算出 → `apply_sfd_fee` で DB に反映する
//! 一連のフローを実 DB で検証。hourly cron 自体の wiring は main.rs に
//! 任せ、ここでは fee 算出 + DB 反映の組み合わせのみテストする。

use auto_trader_core::sfd::{SfdContext, compute_hourly_sfd};
use auto_trader_core::types::Direction;
use auto_trader_db::trades::{apply_sfd_fee, get_trade_by_id};
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::helpers::seed::seed_open_trade;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

async fn insert_btc_open_trade(
    pool: &sqlx::PgPool,
    account_id: uuid::Uuid,
    direction: Direction,
    entry_price: Decimal,
    quantity: Decimal,
) -> uuid::Uuid {
    let dir_str = match direction {
        Direction::Long => "long",
        Direction::Short => "short",
    };
    seed_open_trade(
        pool,
        account_id,
        "test_strat",
        "FX_BTC_JPY",
        "bitflyer_cfd",
        dir_str,
        entry_price,
        entry_price - dec!(1),
        quantity,
        Utc::now(),
    )
    .await
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_bitflyer_10pct_long_pays_hourly_sfd(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "sfd_accr_long",
        "paper",
        "bitflyer_cfd",
        "test_strat",
        1_000_000,
    )
    .await;
    let trade_id =
        insert_btc_open_trade(&pool, account_id, Direction::Long, dec!(36000), dec!(0.01)).await;

    // 10% divergence (FX > spot), Long → 払う
    let fee = compute_hourly_sfd(SfdContext {
        fx_price: dec!(110),
        spot_price: dec!(100),
        position_notional: dec!(36000) * dec!(0.01),
        direction: Direction::Long,
    });
    assert!(fee > Decimal::ZERO);

    let mut tx = pool.begin().await.unwrap();
    let new_balance = apply_sfd_fee(&mut tx, account_id, trade_id, fee)
        .await
        .unwrap()
        .expect("Some");
    tx.commit().await.unwrap();

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, fee);
    assert_eq!(new_balance, dec!(1_000_000) - fee);
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_bitflyer_below_threshold_yields_zero_sfd(pool: sqlx::PgPool) {
    let fee = compute_hourly_sfd(SfdContext {
        fx_price: dec!(104),
        spot_price: dec!(100),
        position_notional: dec!(360),
        direction: Direction::Long,
    });
    assert_eq!(fee, Decimal::ZERO);
    let _ = pool;
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_bitflyer_10pct_short_receives_sfd(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "sfd_accr_short",
        "paper",
        "bitflyer_cfd",
        "test_strat",
        1_000_000,
    )
    .await;
    let trade_id =
        insert_btc_open_trade(&pool, account_id, Direction::Short, dec!(36000), dec!(0.01)).await;

    // 10% divergence (FX > spot), Short → 受け取る
    let fee = compute_hourly_sfd(SfdContext {
        fx_price: dec!(110),
        spot_price: dec!(100),
        position_notional: dec!(360),
        direction: Direction::Short,
    });
    assert!(fee < Decimal::ZERO);

    let mut tx = pool.begin().await.unwrap();
    let new_balance = apply_sfd_fee(&mut tx, account_id, trade_id, fee)
        .await
        .unwrap()
        .expect("Some");
    tx.commit().await.unwrap();

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, fee);
    assert!(trade.fees < Decimal::ZERO);
    assert_eq!(new_balance, dec!(1_000_000) - fee);
    assert!(new_balance > dec!(1_000_000));
}

#[sqlx::test(migrations = "../../migrations")]
async fn apply_sfd_fee_returns_none_when_trade_closed(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "sfd_accr_closed",
        "paper",
        "bitflyer_cfd",
        "test_strat",
        1_000_000,
    )
    .await;
    let trade_id =
        insert_btc_open_trade(&pool, account_id, Direction::Long, dec!(36000), dec!(0.01)).await;
    sqlx::query("UPDATE trades SET status='closed' WHERE id=$1")
        .bind(trade_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let result = apply_sfd_fee(&mut tx, account_id, trade_id, dec!(15))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(result.is_none(), "closed trade must skip apply_sfd_fee");

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, Decimal::ZERO);
}

#[sqlx::test(migrations = "../../migrations")]
async fn account_event_row_recorded_with_sfd_fee_type(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "sfd_accr_event",
        "paper",
        "bitflyer_cfd",
        "test_strat",
        1_000_000,
    )
    .await;
    let trade_id =
        insert_btc_open_trade(&pool, account_id, Direction::Long, dec!(36000), dec!(0.01)).await;

    let mut tx = pool.begin().await.unwrap();
    apply_sfd_fee(&mut tx, account_id, trade_id, dec!(15))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (count, amount, evt_type): (i64, Decimal, String) = sqlx::query_as(
        "SELECT COUNT(*), MAX(amount), MAX(event_type)
         FROM account_events WHERE trade_id=$1",
    )
    .bind(trade_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(amount, dec!(-15));
    assert_eq!(evt_type, "sfd_fee");
}
