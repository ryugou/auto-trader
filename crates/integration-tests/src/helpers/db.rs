//! DB テスト用ヘルパー — snapshot / seed 関数群。

use sqlx::PgPool;
use uuid::Uuid;

/// `seed_standard_accounts` が返す構造体。
pub struct StandardAccounts {
    pub bitflyer_cfd_account_id: Uuid,
    pub gmo_fx_account_id: Uuid,
}

/// 指定テーブルの全行を JSON 文字列としてダンプする。
/// テスト失敗時の診断ログに使う想定。
pub async fn snapshot_tables(pool: &PgPool, tables: &[&str]) -> String {
    const ALLOWED_TABLES: &[&str] = &[
        "trading_accounts", "trades", "price_candles", "daily_summary",
        "strategies", "strategy_params", "notifications", "account_events",
        "macro_events",
    ];

    let mut out = String::new();
    for table in tables {
        assert!(
            ALLOWED_TABLES.contains(table),
            "snapshot_tables: table name '{table}' is not in the allowlist"
        );
        let query = format!(
            "SELECT json_agg(t) FROM (SELECT * FROM {table}) t",
        );
        let row: (Option<serde_json::Value>,) = sqlx::query_as(&query)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("snapshot_tables: failed to query {table}: {e}"));

        out.push_str(&format!("=== {table} ===\n"));
        match row.0 {
            Some(val) => out.push_str(&serde_json::to_string_pretty(&val).unwrap()),
            None => out.push_str("(empty)"),
        }
        out.push('\n');
    }
    out
}

/// テスト用の strategy 行を seed する（FK 制約を満たすため）。
/// 既に存在する場合は何もしない。
async fn ensure_strategy(pool: &PgPool, name: &str) {
    sqlx::query(
        r#"INSERT INTO strategies (name, display_name, category, risk_level, description, default_params)
           VALUES ($1, $1, 'test', 'low', 'test strategy', '{}'::jsonb)
           ON CONFLICT (name) DO NOTHING"#,
    )
    .bind(name)
    .execute(pool)
    .await
    .expect("ensure_strategy: insert failed");
}

/// テスト用 trading_account を 1 行挿入し、生成された UUID を返す。
pub async fn seed_trading_account(
    pool: &PgPool,
    name: &str,
    account_type: &str,
    exchange: &str,
    strategy: &str,
    initial_balance: i64,
) -> Uuid {
    ensure_strategy(pool, strategy).await;

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
    .expect("seed_trading_account: insert failed");

    id
}

/// BitflyerCfd + GmoFx の paper アカウントを seed する。
pub async fn seed_standard_accounts(pool: &PgPool) -> StandardAccounts {
    let bitflyer_cfd_account_id = seed_trading_account(
        pool,
        "test_bitflyer_cfd",
        "paper",
        "bitflyer_cfd",
        "bb_mean_revert_v1",
        30_000,
    )
    .await;

    let gmo_fx_account_id = seed_trading_account(
        pool,
        "test_gmo_fx",
        "paper",
        "gmo_fx",
        "donchian_trend_v1",
        30_000,
    )
    .await;

    StandardAccounts {
        bitflyer_cfd_account_id,
        gmo_fx_account_id,
    }
}
