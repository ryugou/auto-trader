//! Phase 3: paper GMO FX swap accrual の DB レベル統合テスト。
//!
//! `compute_daily_swap` で fee を算出 → `apply_swap_fee` で DB に反映する
//! 一連のフローを実 DB で検証。cron 自体の wiring は main.rs に任せ、
//! ここでは fee 算出 + DB 反映の組み合わせのみテストする
//! (phase3_sfd_paper_accrual.rs と同パターン)。
//!
//! sign 規約 (compute_daily_swap / apply_swap_fee 整合):
//!   config rate > 0 = paper 払い → fees 増、balance 減
//!   config rate < 0 = paper 受取 → fees 減、balance 増

use auto_trader_core::config::SwapRateEntry;
use auto_trader_core::swap::compute_daily_swap;
use auto_trader_core::types::Direction;
use auto_trader_db::trades::{apply_swap_fee, get_trade_by_id};
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::helpers::seed::seed_open_trade;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

async fn insert_usdjpy_trade(
    pool: &sqlx::PgPool,
    account_id: uuid::Uuid,
    direction: Direction,
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
        "USD_JPY",
        "gmo_fx",
        dir_str,
        dec!(150),
        dec!(149),
        quantity,
        Utc::now(),
    )
    .await
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_gmo_long_pays_swap(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "swap_long_pay",
        "paper",
        "gmo_fx",
        "test_strat",
        1_000_000,
    )
    .await;
    let trade_id = insert_usdjpy_trade(&pool, account_id, Direction::Long, dec!(10_000)).await;

    // config: USD_JPY long = +100 (paper 払い), short = -120 (paper 受取)
    // Long 1 lot → fee = +100 (paper 払い)
    let rate = SwapRateEntry {
        long: dec!(100),
        short: dec!(-120),
    };
    let fee = compute_daily_swap(rate, Direction::Long, dec!(10_000));
    assert_eq!(fee, dec!(100));

    let mut tx = pool.begin().await.unwrap();
    let new_balance = apply_swap_fee(&mut tx, account_id, trade_id, fee, Utc::now())
        .await
        .unwrap()
        .expect("Some");
    tx.commit().await.unwrap();

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, dec!(100), "paper 払いで fees 増");
    assert_eq!(new_balance, dec!(999_900), "paper 払いで balance 減");
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_gmo_short_receives_swap(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "swap_short_receive",
        "paper",
        "gmo_fx",
        "test_strat",
        1_000_000,
    )
    .await;
    let trade_id = insert_usdjpy_trade(&pool, account_id, Direction::Short, dec!(10_000)).await;

    // Short 1 lot → fee = -120 (paper 受取)
    let rate = SwapRateEntry {
        long: dec!(100),
        short: dec!(-120),
    };
    let fee = compute_daily_swap(rate, Direction::Short, dec!(10_000));
    assert_eq!(fee, dec!(-120));

    let mut tx = pool.begin().await.unwrap();
    let new_balance = apply_swap_fee(&mut tx, account_id, trade_id, fee, Utc::now())
        .await
        .unwrap()
        .expect("Some");
    tx.commit().await.unwrap();

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, dec!(-120), "paper 受取で fees 減 (負値)");
    assert_eq!(
        new_balance,
        dec!(1_000_120),
        "paper 受取で balance 増 (1_000_000 - (-120) = 1_000_120)"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn apply_swap_fee_returns_none_when_trade_closed(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "swap_closed",
        "paper",
        "gmo_fx",
        "test_strat",
        1_000_000,
    )
    .await;
    let trade_id = insert_usdjpy_trade(&pool, account_id, Direction::Long, dec!(10_000)).await;
    sqlx::query("UPDATE trades SET status='closed' WHERE id=$1")
        .bind(trade_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let result = apply_swap_fee(&mut tx, account_id, trade_id, dec!(100), Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(result.is_none(), "closed trade must skip apply_swap_fee");

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, Decimal::ZERO);
}

#[sqlx::test(migrations = "../../migrations")]
async fn account_event_row_recorded_with_swap_fee_type(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "swap_event",
        "paper",
        "gmo_fx",
        "test_strat",
        1_000_000,
    )
    .await;
    let trade_id = insert_usdjpy_trade(&pool, account_id, Direction::Long, dec!(10_000)).await;

    let mut tx = pool.begin().await.unwrap();
    apply_swap_fee(&mut tx, account_id, trade_id, dec!(100), Utc::now())
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
    assert_eq!(amount, dec!(-100), "払い時 amount は -fee");
    assert_eq!(evt_type, "swap_fee");
}
