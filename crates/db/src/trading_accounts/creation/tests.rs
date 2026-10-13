use rust_decimal_macros::dec;
use uuid::Uuid;

use super::*;

mod leverage_validation {
    use auto_trader_core::types::Exchange;
    use rust_decimal_macros::dec;

    use super::validate_leverage_for_exchange;

    #[test]
    fn gmo_fx_accepts_up_to_25x() {
        assert!(validate_leverage_for_exchange(Exchange::GmoFx, dec!(25)).is_ok());
        assert!(validate_leverage_for_exchange(Exchange::GmoFx, dec!(1)).is_ok());
    }

    #[test]
    fn gmo_fx_rejects_above_25x() {
        let err = validate_leverage_for_exchange(Exchange::GmoFx, dec!(26)).unwrap_err();
        assert_eq!(err, "invalid leverage: exceeds regulatory cap 25");
    }

    #[test]
    fn bitflyer_cfd_accepts_up_to_2x() {
        assert!(validate_leverage_for_exchange(Exchange::BitflyerCfd, dec!(2)).is_ok());
        assert!(validate_leverage_for_exchange(Exchange::BitflyerCfd, dec!(1)).is_ok());
    }

    #[test]
    fn bitflyer_cfd_rejects_above_2x() {
        let err = validate_leverage_for_exchange(Exchange::BitflyerCfd, dec!(3)).unwrap_err();
        assert_eq!(err, "invalid leverage: exceeds regulatory cap 2");
    }

    #[test]
    fn oanda_currently_uncapped() {
        assert!(validate_leverage_for_exchange(Exchange::Oanda, dec!(100)).is_ok());
    }
}

mod initial_balance_validation {
    use rust_decimal_macros::dec;

    use super::{MIN_INITIAL_BALANCE_JPY, validate_initial_balance};

    #[test]
    fn jpy_rejects_below_minimum() {
        let err = validate_initial_balance("JPY", MIN_INITIAL_BALANCE_JPY - dec!(1)).unwrap_err();
        assert!(err.contains("10000"), "error mentions the minimum: {err}");
    }

    #[test]
    fn jpy_accepts_at_minimum() {
        assert!(validate_initial_balance("JPY", MIN_INITIAL_BALANCE_JPY).is_ok());
    }
}

pub(in crate::trading_accounts) fn live_req(exchange: &str) -> CreateTradingAccount {
    CreateTradingAccount {
        name: format!("live-{exchange}"),
        exchange: exchange.to_string(),
        initial_balance: dec!(50000),
        leverage: dec!(1),
        strategy: "bb_mean_revert_v1".to_string(),
        account_type: "live".to_string(),
        currency: "JPY".to_string(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_insert_same_exchange_succeeds_after_existing_retired(pool: sqlx::PgPool) {
    let first = create_account(&pool, &live_req("bitflyer_cfd"))
        .await
        .expect("first live insert ok");
    sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
        .bind(first.id)
        .execute(&pool)
        .await
        .expect("retire first live account");

    create_account(&pool, &live_req("bitflyer_cfd"))
        .await
        .expect("second live insert should succeed once the first is retired");
}

#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_live_insert_same_exchange_fails(pool: sqlx::PgPool) {
    let req = live_req("bitflyer_cfd");
    create_account(&pool, &req).await.expect("first insert ok");
    let err = create_account(&pool, &req)
        .await
        .expect_err("second live insert should fail");
    assert_eq!(
        err.to_string(),
        build_one_live_per_exchange_conflict_message(CREATE_ACCOUNT_OPERATION, "bitflyer_cfd"),
        "unexpected error: {err}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn translate_unique_violation_reports_already_exists(pool: sqlx::PgPool) {
    create_account(&pool, &live_req("bitflyer_cfd"))
        .await
        .expect("create first live account");

    let db_err = sqlx::query(
        "INSERT INTO trading_accounts
             (id, name, account_type, exchange, strategy,
              initial_balance, current_balance, leverage, currency, active)
         VALUES ($1, 'second-live', 'live', 'bitflyer_cfd', 'bb_mean_revert_v1',
                 50000, 50000, 1, 'JPY', TRUE)",
    )
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .expect_err("a second active live row for the same exchange must violate the unique index");
    assert!(
        matches!(db_err, sqlx::Error::Database(_)),
        "expected a database error from the unique index, got: {db_err:?}"
    );

    let translated = translate_unique_violation(db_err, CREATE_ACCOUNT_OPERATION, "bitflyer_cfd");
    assert_eq!(
        translated.to_string(),
        build_one_live_per_exchange_conflict_message(CREATE_ACCOUNT_OPERATION, "bitflyer_cfd"),
        "unexpected message: {translated}"
    );
    assert!(
        translated.chain().any(|c| matches!(
            c.downcast_ref::<sqlx::Error>(),
            Some(sqlx::Error::Database(e))
                if e.constraint() == Some("trading_accounts_one_live_per_exchange")
        )),
        "the original sqlx::Error::Database must remain in the anyhow chain"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_insert_different_exchange_succeeds(pool: sqlx::PgPool) {
    create_account(&pool, &live_req("bitflyer_cfd"))
        .await
        .expect("first exchange ok");
    create_account(&pool, &live_req("oanda"))
        .await
        .expect("different exchange should succeed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_rejects_unknown_exchange(pool: sqlx::PgPool) {
    let req = CreateTradingAccount {
        name: "bad-exchange".to_string(),
        exchange: "unknown".to_string(),
        initial_balance: dec!(50000),
        leverage: dec!(1),
        strategy: "bb_mean_revert_v1".to_string(),
        account_type: "paper".to_string(),
        currency: "JPY".to_string(),
    };
    let err = create_account(&pool, &req)
        .await
        .expect_err("unknown exchange should be rejected");
    assert!(
        err.to_string().contains("unsupported exchange: unknown"),
        "unexpected error: {err}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_rejects_invalid_account_type(pool: sqlx::PgPool) {
    let req = CreateTradingAccount {
        name: "bad-account-type".to_string(),
        exchange: "gmo_fx".to_string(),
        initial_balance: dec!(50000),
        leverage: dec!(1),
        strategy: "bb_mean_revert_v1".to_string(),
        account_type: "demo".to_string(),
        currency: "JPY".to_string(),
    };
    let err = create_account(&pool, &req)
        .await
        .expect_err("invalid account_type should be rejected");
    assert!(
        err.to_string().contains("invalid account_type"),
        "unexpected error: {err}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_rejects_malformed_exchange(pool: sqlx::PgPool) {
    let req = CreateTradingAccount {
        name: "bad-exchange-chars".to_string(),
        exchange: "GMO-FX!".to_string(),
        initial_balance: dec!(50000),
        leverage: dec!(1),
        strategy: "bb_mean_revert_v1".to_string(),
        account_type: "paper".to_string(),
        currency: "JPY".to_string(),
    };
    let err = create_account(&pool, &req)
        .await
        .expect_err("malformed exchange should be rejected");
    assert!(
        err.to_string().contains("invalid exchange"),
        "unexpected error: {err}"
    );
}
