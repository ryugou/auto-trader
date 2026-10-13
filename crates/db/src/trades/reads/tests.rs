use chrono::TimeZone;
use rust_decimal_macros::dec;

use super::*;

#[allow(clippy::too_many_arguments)]
async fn insert_test_trade(
    pool: &PgPool,
    account_id: Uuid,
    strategy_name: &str,
    status: &str,
    entry_at: DateTime<Utc>,
    exit_at: Option<DateTime<Utc>>,
    pnl_amount: Option<Decimal>,
    fees: Decimal,
) {
    sqlx::query(
        r#"INSERT INTO trades
               (id, account_id, strategy_name, pair, exchange, direction,
                entry_price, quantity, leverage, fees, stop_loss,
                entry_at, exit_at, pnl_amount, status)
           VALUES ($1, $2, $3, 'FX_BTC_JPY', 'bitflyer_cfd', 'long',
                   100, 0.01, 2, $4, 90,
                   $5, $6, $7, $8)"#,
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(strategy_name)
    .bind(fees)
    .bind(entry_at)
    .bind(exit_at)
    .bind(pnl_amount)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert test trade");
}

#[sqlx::test(migrations = "../../migrations")]
async fn realized_net_since_sums_closed_trades_and_excludes_boundary(pool: sqlx::PgPool) {
    let account = crate::trading_accounts::list_all(&pool)
        .await
        .expect("list accounts")
        .into_iter()
        .next()
        .expect("migrations seed at least one account");
    let strategy = account.strategy.clone();

    let since = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();

    insert_test_trade(
        &pool,
        account.id,
        &strategy,
        "closed",
        Utc.with_ymd_and_hms(2026, 7, 5, 0, 0, 0).unwrap(),
        Some(Utc.with_ymd_and_hms(2026, 7, 5, 12, 0, 0).unwrap()),
        Some(dec!(1000)),
        dec!(100),
    )
    .await;
    insert_test_trade(
        &pool,
        account.id,
        &strategy,
        "closed",
        Utc.with_ymd_and_hms(2026, 7, 6, 0, 0, 0).unwrap(),
        Some(Utc.with_ymd_and_hms(2026, 7, 6, 12, 0, 0).unwrap()),
        Some(dec!(-300)),
        dec!(50),
    )
    .await;
    insert_test_trade(
        &pool,
        account.id,
        &strategy,
        "closed",
        Utc.with_ymd_and_hms(2026, 6, 30, 0, 0, 0).unwrap(),
        Some(Utc.with_ymd_and_hms(2026, 6, 30, 23, 59, 59).unwrap()),
        Some(dec!(99999)),
        dec!(0),
    )
    .await;
    insert_test_trade(
        &pool,
        account.id,
        &strategy,
        "open",
        Utc.with_ymd_and_hms(2026, 7, 7, 0, 0, 0).unwrap(),
        None,
        None,
        dec!(0),
    )
    .await;

    let net = realized_net_since(&pool, account.id, since)
        .await
        .expect("realized_net_since");
    assert_eq!(net, dec!(550), "only closed trades at/after `since` count");
}

#[sqlx::test(migrations = "../../migrations")]
async fn realized_net_since_zero_when_no_trades(pool: sqlx::PgPool) {
    let account = crate::trading_accounts::list_all(&pool)
        .await
        .expect("list accounts")
        .into_iter()
        .next()
        .expect("migrations seed at least one account");
    let since = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
    let net = realized_net_since(&pool, account.id, since)
        .await
        .expect("realized_net_since");
    assert_eq!(net, Decimal::ZERO);
}
