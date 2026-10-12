use rust_decimal_macros::dec;

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
        assert!(err.contains("25"), "error mentions cap: {err}");
        assert!(err.contains("gmo_fx"), "error mentions exchange: {err}");
    }

    #[test]
    fn bitflyer_cfd_accepts_up_to_2x() {
        assert!(validate_leverage_for_exchange(Exchange::BitflyerCfd, dec!(2)).is_ok());
        assert!(validate_leverage_for_exchange(Exchange::BitflyerCfd, dec!(1)).is_ok());
    }

    #[test]
    fn bitflyer_cfd_rejects_above_2x() {
        let err = validate_leverage_for_exchange(Exchange::BitflyerCfd, dec!(3)).unwrap_err();
        assert!(err.contains("2"), "error mentions cap: {err}");
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

fn live_req(exchange: &str) -> CreateTradingAccount {
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

fn paper_req(name: &str, exchange: &str) -> CreateTradingAccount {
    CreateTradingAccount {
        name: name.to_string(),
        exchange: exchange.to_string(),
        initial_balance: dec!(50000),
        leverage: dec!(1),
        strategy: "bb_mean_revert_v1".to_string(),
        account_type: "paper".to_string(),
        currency: "JPY".to_string(),
    }
}

fn no_op_update() -> UpdateTradingAccount {
    UpdateTradingAccount {
        name: None,
        leverage: None,
        strategy: None,
        active: None,
    }
}

async fn insert_trade_with_status(pool: &sqlx::PgPool, account_id: Uuid, status: &str) {
    sqlx::query(
        r#"INSERT INTO trades
               (id, account_id, strategy_name, pair, exchange, direction,
                entry_price, quantity, leverage, stop_loss, entry_at, status)
           VALUES ($1, $2, 'bb_mean_revert_v1', 'FX_BTC_JPY', 'bitflyer_cfd', 'long',
                   100, 0.01, 2, 90, NOW(), $3)"#,
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert trade with status");
}

async fn insert_open_trade(pool: &sqlx::PgPool, account_id: Uuid) {
    insert_trade_with_status(pool, account_id, "open").await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn list_all_includes_inactive_accounts(pool: sqlx::PgPool) {
    let retired_id = list_all(&pool)
        .await
        .expect("list accounts")
        .first()
        .expect("migrations seed at least one account")
        .id;

    sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
        .bind(retired_id)
        .execute(&pool)
        .await
        .expect("deactivate account");

    let listed = list_all(&pool).await.expect("list accounts after retire");
    assert!(
        listed.iter().any(|a| a.id == retired_id),
        "list_all must still include the now-inactive account {retired_id}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn list_active_excludes_inactive_accounts(pool: sqlx::PgPool) {
    let retired_id = list_active(&pool)
        .await
        .expect("list accounts")
        .first()
        .expect("migrations seed at least one active account")
        .id;

    sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
        .bind(retired_id)
        .execute(&pool)
        .await
        .expect("deactivate account");

    let listed = list_active(&pool)
        .await
        .expect("list accounts after retire");
    assert!(
        listed.iter().all(|a| a.id != retired_id),
        "inactive account {retired_id} must not appear in list_active"
    );
    let fetched = get(&pool, retired_id)
        .await
        .expect("get account")
        .expect("get returns inactive accounts");
    assert!(!fetched.active, "fetched account must be inactive");
}

#[sqlx::test(migrations = "../../migrations")]
async fn list_active_or_with_open_trades_covers_active_and_retired_with_open_trades(
    pool: sqlx::PgPool,
) {
    let active_id = create_account(&pool, &paper_req("active_acct", "gmo_fx"))
        .await
        .expect("create active account")
        .id;
    let retired_no_trades_id = create_account(&pool, &paper_req("retired_no_trades", "gmo_fx"))
        .await
        .expect("create retired-no-trades account")
        .id;
    let retired_with_trade_id = create_account(&pool, &paper_req("retired_with_trade", "gmo_fx"))
        .await
        .expect("create retired-with-trade account")
        .id;

    for id in [retired_no_trades_id, retired_with_trade_id] {
        sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("deactivate account");
    }
    insert_open_trade(&pool, retired_with_trade_id).await;

    let active_only = list_active(&pool).await.expect("list_active");
    assert!(
        active_only.iter().any(|a| a.id == active_id),
        "list_active must include the active account"
    );
    assert!(
        active_only
            .iter()
            .all(|a| a.id != retired_no_trades_id && a.id != retired_with_trade_id),
        "list_active must exclude every retired account regardless of open trades"
    );

    let active_or_open = list_active_or_with_open_trades(&pool)
        .await
        .expect("list_active_or_with_open_trades");
    assert!(
        active_or_open.iter().any(|a| a.id == active_id),
        "must include the active account"
    );
    assert!(
        active_or_open.iter().any(|a| a.id == retired_with_trade_id),
        "must include the retired account that still has an open trade"
    );
    assert!(
        active_or_open.iter().all(|a| a.id != retired_no_trades_id),
        "must exclude the retired account with no open trades"
    );

    let all = list_all(&pool).await.expect("list_all");
    assert!(
        all.iter().any(|a| a.id == retired_no_trades_id),
        "list_all must include every account regardless of trades"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn list_active_or_with_open_trades_includes_retired_account_with_closing_trade(
    pool: sqlx::PgPool,
) {
    let retired_closing_id = create_account(&pool, &paper_req("retired_closing", "gmo_fx"))
        .await
        .expect("create retired-closing account")
        .id;
    sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
        .bind(retired_closing_id)
        .execute(&pool)
        .await
        .expect("deactivate account");
    insert_trade_with_status(&pool, retired_closing_id, "closing").await;

    let listed = list_active_or_with_open_trades(&pool)
        .await
        .expect("list_active_or_with_open_trades");
    assert!(
        listed.iter().any(|a| a.id == retired_closing_id),
        "must include the retired account that still has a closing trade"
    );
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
async fn update_account_rejects_deactivation_with_open_trades(pool: sqlx::PgPool) {
    let account = create_account(&pool, &paper_req("has_open_trade", "gmo_fx"))
        .await
        .expect("create account");
    insert_open_trade(&pool, account.id).await;

    let err = update_account(
        &pool,
        account.id,
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect_err("deactivation with an open trade must be rejected");
    assert!(
        err.to_string().contains("1 open/closing trade"),
        "error must report the open/closing trade count: {err}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_rejects_deactivation_with_open_trades_as_typed_error(pool: sqlx::PgPool) {
    let account = create_account(&pool, &paper_req("typed_open_trade_err", "gmo_fx"))
        .await
        .expect("create account");
    insert_open_trade(&pool, account.id).await;

    let err = update_account(
        &pool,
        account.id,
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect_err("deactivation with an open trade must be rejected");
    assert!(
        matches!(err, UpdateAccountError::OpenTrades { count: 1, .. }),
        "expected UpdateAccountError::OpenTrades {{ count: 1, .. }}, got: {err:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_rejects_deactivation_with_closing_trades(pool: sqlx::PgPool) {
    let account = create_account(&pool, &paper_req("has_closing_trade", "gmo_fx"))
        .await
        .expect("create account");
    insert_trade_with_status(&pool, account.id, "closing").await;

    let err = update_account(
        &pool,
        account.id,
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect_err("deactivation with a closing trade must be rejected");
    assert!(
        err.to_string().contains("1 open/closing trade"),
        "error must report the closing trade: {err}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_rejects_deactivation_counts_open_and_closing_together(pool: sqlx::PgPool) {
    let account = create_account(&pool, &paper_req("has_both_statuses", "gmo_fx"))
        .await
        .expect("create account");
    insert_trade_with_status(&pool, account.id, "open").await;
    insert_trade_with_status(&pool, account.id, "closing").await;

    let err = update_account(
        &pool,
        account.id,
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect_err("deactivation with open + closing trades must be rejected");
    assert!(
        err.to_string().contains("2 open/closing trade"),
        "error must report the combined count of 2: {err}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_returns_none_for_nonexistent_account_when_deactivating(pool: sqlx::PgPool) {
    let result = update_account(
        &pool,
        Uuid::new_v4(),
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect("a nonexistent account must not error");
    assert!(
        result.is_none(),
        "nonexistent account must report Ok(None), not an error"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_allows_deactivation_without_open_trades(pool: sqlx::PgPool) {
    let account = create_account(&pool, &paper_req("no_open_trade", "gmo_fx"))
        .await
        .expect("create account");

    let updated = update_account(
        &pool,
        account.id,
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect("deactivation should succeed")
    .expect("account exists");
    assert!(!updated.active);
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_allows_repeated_deactivation_with_open_trades(pool: sqlx::PgPool) {
    let account = create_account(&pool, &paper_req("repeat_retire", "gmo_fx"))
        .await
        .expect("create account");

    update_account(
        &pool,
        account.id,
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect("first deactivation should succeed")
    .expect("account exists");

    insert_open_trade(&pool, account.id).await;

    let updated = update_account(
        &pool,
        account.id,
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect("repeated deactivation on an already-retired account must succeed")
    .expect("account exists");
    assert!(!updated.active, "account must remain inactive");
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_rejects_reactivating_live_when_another_active_live_exists(
    pool: sqlx::PgPool,
) {
    let retired = create_account(&pool, &live_req("bitflyer_cfd"))
        .await
        .expect("create first live account");
    update_account(
        &pool,
        retired.id,
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect("retire first live account");
    create_account(&pool, &live_req("bitflyer_cfd"))
        .await
        .expect("create second live account while first is retired");

    let err = update_account(
        &pool,
        retired.id,
        &UpdateTradingAccount {
            active: Some(true),
            ..no_op_update()
        },
    )
    .await
    .expect_err("reactivating must be rejected while another live account is active");
    assert!(
        err.to_string().contains("already active"),
        "unexpected error: {err}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_allows_reactivating_live_when_no_conflict(pool: sqlx::PgPool) {
    let account = create_account(&pool, &live_req("bitflyer_cfd"))
        .await
        .expect("create live account");
    update_account(
        &pool,
        account.id,
        &UpdateTradingAccount {
            active: Some(false),
            ..no_op_update()
        },
    )
    .await
    .expect("retire account");

    let reactivated = update_account(
        &pool,
        account.id,
        &UpdateTradingAccount {
            active: Some(true),
            ..no_op_update()
        },
    )
    .await
    .expect("reactivation should succeed")
    .expect("account exists");
    assert!(reactivated.active);
}

#[sqlx::test(migrations = "../../migrations")]
async fn migration_retires_legacy_fx_accounts_and_seeds_fx_new(pool: sqlx::PgPool) {
    let listed = list_active(&pool).await.expect("list accounts");
    let fx_new_id = Uuid::parse_str("a0000000-0000-0000-0000-000000000040").unwrap();

    let fx_new = listed
        .iter()
        .find(|a| a.id == fx_new_id)
        .expect("FX新 account is seeded and active");
    assert_eq!(fx_new.name, "FX新");
    assert_eq!(fx_new.strategy, "fx_new_v1");
    assert_eq!(fx_new.exchange, "gmo_fx");
    assert_eq!(fx_new.account_type, "paper");
    assert_eq!(fx_new.leverage, dec!(10));
    assert!(fx_new.active);

    for legacy in [
        "a0000000-0000-0000-0000-000000000031",
        "a0000000-0000-0000-0000-000000000032",
    ] {
        let id = Uuid::parse_str(legacy).unwrap();
        assert!(
            listed.iter().all(|a| a.id != id),
            "legacy account {id} must not be listed"
        );
        if let Some(account) = get(&pool, id).await.expect("get legacy account") {
            assert!(!account.active, "legacy account {id} must be inactive");
        }
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_live_insert_same_exchange_fails(pool: sqlx::PgPool) {
    let req = live_req("bitflyer_cfd");
    create_account(&pool, &req).await.expect("first insert ok");
    let err = create_account(&pool, &req)
        .await
        .expect_err("second live insert should fail");
    assert!(
        err.to_string().contains("already exists"),
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

    let translated = translate_unique_violation(db_err);
    assert!(
        translated.to_string().contains("already exists"),
        "message must say the live account already exists: {translated}"
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
async fn set_and_get_halt_roundtrip(pool: sqlx::PgPool) {
    use chrono::TimeZone;
    let account = list_all(&pool)
        .await
        .expect("list accounts")
        .into_iter()
        .next()
        .expect("migrations seed at least one account");

    assert!(
        get_halt(&pool, account.id).await.unwrap().is_none(),
        "no halt set initially"
    );

    let until = chrono::Utc.with_ymd_and_hms(2026, 7, 8, 0, 0, 0).unwrap();
    set_halt(&pool, account.id, until, "daily loss limit")
        .await
        .expect("set_halt");

    let (got_until, got_reason) = get_halt(&pool, account.id)
        .await
        .expect("get_halt")
        .expect("halt should be set");
    assert_eq!(got_until, until);
    assert_eq!(got_reason.as_deref(), Some("daily loss limit"));
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

#[sqlx::test(migrations = "../../migrations")]
async fn recalculate_balance_rejects_when_open_trades_exist(pool: sqlx::PgPool) {
    let account = create_account(&pool, &paper_req("recalc_open_trade", "gmo_fx"))
        .await
        .expect("create account");
    insert_open_trade(&pool, account.id).await;

    let err = recalculate_balance(&pool, account.id)
        .await
        .expect_err("recalculation with an open trade must be rejected");
    assert!(
        err.to_string().contains("open/closing trade"),
        "unexpected error: {err}"
    );
}
