use anyhow::Context;
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

/// Ids of the accounts that `seed_standard_accounts` inserts.
pub struct StandardAccounts {
    /// Id of the paper account on bitFlyer CFD.
    pub bitflyer_cfd_account_id: Uuid,
    /// Id of the paper account on GMO FX.
    pub gmo_fx_account_id: Uuid,
}

/// Dumps every row of each given table as pretty-printed JSON, under a header line per table; a table without rows is dumped as `(empty)`.
///
/// # Errors
///
/// - Returns an error if the query of a table fails.
/// - Returns an error if serializing the rows of a table fails.
///
/// # Panics
///
/// Panics if a table is not one of `trading_accounts`, `trades`, `price_candles`, `daily_summary`, `strategies`, `strategy_params`, `notifications`, `account_events` and `macro_events`.
pub async fn snapshot_tables(pool: &PgPool, tables: &[&str]) -> anyhow::Result<String> {
    const ALLOWED_TABLES: &[&str] = &[
        "trading_accounts",
        "trades",
        "price_candles",
        "daily_summary",
        "strategies",
        "strategy_params",
        "notifications",
        "account_events",
        "macro_events",
    ];

    let mut out = String::new();
    for table in tables {
        assert!(
            ALLOWED_TABLES.contains(table),
            "unsupported table: \"{table}\""
        );
        // SAFETY: table name is validated against ALLOWED_TABLES above; no user input.
        let query = format!("SELECT json_agg(t) FROM (SELECT * FROM {table}) t",);
        let row: (Option<serde_json::Value>,) = sqlx::query_as(&query)
            .fetch_one(pool)
            .await
            .with_context(|| format!("failed to query table \"{table}\""))?;

        out.push_str(&format!("=== {table} ===\n"));
        match row.0 {
            Some(val) => out.push_str(
                &serde_json::to_string_pretty(&val)
                    .with_context(|| format!("failed to serialize table \"{table}\""))?,
            ),
            None => out.push_str("(empty)"),
        }
        out.push('\n');
    }
    Ok(out)
}

async fn ensure_strategy_for_foreign_key(pool: &PgPool, name: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO strategies (name, display_name, category, risk_level, description, default_params)
           VALUES ($1, $1, 'test', 'low', 'test strategy', '{}'::jsonb)
           ON CONFLICT (name) DO NOTHING"#,
    )
    .bind(name)
    .execute(pool)
    .await
    .context("failed to insert strategy")?;
    Ok(())
}

/// Inserts a trading account with a JPY currency and a leverage of 2, whose current balance equals `initial_balance`, and returns its id.
///
/// A strategy named `strategy` is inserted first when none exists.
///
/// # Errors
///
/// - Returns an error if inserting the strategy fails.
/// - Returns an error if inserting the account fails.
pub async fn seed_trading_account(
    pool: &PgPool,
    name: &str,
    account_type: &str,
    exchange: &str,
    strategy: &str,
    initial_balance: i64,
) -> anyhow::Result<Uuid> {
    ensure_strategy_for_foreign_key(pool, strategy).await?;

    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO trading_accounts
               (id, name, account_type, exchange, strategy,
                initial_balance, current_balance, leverage, currency)
           VALUES ($1, $2, $3, $4, $5, $6, $6, 2, 'JPY')"#,
    )
    .bind(id)
    .bind(name)
    .bind(account_type)
    .bind(exchange)
    .bind(strategy)
    .bind(initial_balance)
    .execute(pool)
    .await
    .context("failed to insert trading account")?;

    Ok(id)
}

/// Returns the current balance of the given account.
///
/// # Errors
///
/// - Returns an error if the query fails or the account does not exist.
pub async fn read_current_balance(pool: &PgPool, account_id: Uuid) -> anyhow::Result<Decimal> {
    sqlx::query_scalar("SELECT current_balance FROM trading_accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .context("failed to read current balance")
}

/// Inserts a paper account on bitFlyer CFD and one on GMO FX, each with an initial balance of 30000, and returns their ids.
///
/// # Errors
///
/// - Returns an error if inserting the strategy of either account fails.
/// - Returns an error if inserting either account fails.
pub async fn seed_standard_accounts(pool: &PgPool) -> anyhow::Result<StandardAccounts> {
    let bitflyer_cfd_account_id = seed_trading_account(
        pool,
        "test_bitflyer_cfd",
        "paper",
        "bitflyer_cfd",
        "bb_mean_revert_v1",
        30_000,
    )
    .await?;

    let gmo_fx_account_id = seed_trading_account(
        pool,
        "test_gmo_fx",
        "paper",
        "gmo_fx",
        "donchian_trend_v1",
        30_000,
    )
    .await?;

    Ok(StandardAccounts {
        bitflyer_cfd_account_id,
        gmo_fx_account_id,
    })
}
