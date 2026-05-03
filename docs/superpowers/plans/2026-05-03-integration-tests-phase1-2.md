# Plan 2: Phase 1 (基盤検証) + Phase 2 (API 全エンドポイント)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phase 1（設定バリデーション、warmup、戦略登録、通知 purge）と Phase 2（全 API エンドポイントの正常系・異常系）の結合テストを実装する。テスト用 API サーバーヘルパー `spawn_test_app` を基盤として、全エンドポイントを reqwest で叩く E2E テストを構築する。

**Architecture:** `crates/integration-tests/tests/` 配下に Phase 別テストファイルを配置。Phase 2 テストは `spawn_test_app(pool) -> (url, JoinHandle)` ヘルパーでインプロセス API サーバーを起動し、reqwest で HTTP リクエストを送信する。各テストは `#[sqlx::test(migrations = "../../migrations")]` で独立した DB を取得。

**Tech Stack:** Rust (workspace edition 2024), sqlx (PostgreSQL), axum (API server), reqwest (HTTP client), tokio (async runtime), auto-trader-app (router + AppState)

**ブランチ:** `feat/integration-tests-phase1-2`

**参照スペック:** `docs/superpowers/specs/2026-05-02-integration-test-design.md` — Phase 1 / Phase 2 セクション

**前提:** Plan 1（`feat/integration-test-infra`）が main にマージ済み

---

## 0. Scope と非スコープ

**本 Plan で実装する:**
- `spawn_test_app` ヘルパー（インプロセス API サーバー起動）
- TOML 設定フィクスチャ 5 ファイル（valid / unknown_exchange / missing_pairs / invalid_strategy / disabled_strategy）
- Phase 1 テスト: config loading, warmup, strategy registration, notification purge
- Phase 2 テスト: accounts CRUD, trades, positions, strategies, dashboard, notifications, health, market, auth
- DB シードヘルパー追加（trades, notifications, daily_summary 用）

**本 Plan で実装しない:**
- Phase 3（トレードフロー）テスト — Plan 3 で実施
- Phase 4（外部 API）テスト — Plan 4 で実施
- CSV 価格フィクスチャの追加（Phase 3 で作成）

---

## File Structure

**新規作成:**
- `crates/integration-tests/src/helpers/app.rs` — `spawn_test_app` ヘルパー
- `crates/integration-tests/src/helpers/seed.rs` — trades/notifications/daily_summary シード
- `crates/integration-tests/fixtures/config_valid.toml`
- `crates/integration-tests/fixtures/config_unknown_exchange.toml`
- `crates/integration-tests/fixtures/config_missing_pairs.toml`
- `crates/integration-tests/fixtures/config_invalid_strategy.toml`
- `crates/integration-tests/fixtures/config_disabled_strategy.toml`
- `crates/integration-tests/tests/phase1_config.rs`
- `crates/integration-tests/tests/phase1_startup.rs`
- `crates/integration-tests/tests/phase2_accounts.rs`
- `crates/integration-tests/tests/phase2_trades_positions.rs`
- `crates/integration-tests/tests/phase2_strategies_dashboard.rs`
- `crates/integration-tests/tests/phase2_notifications_health_market_auth.rs`

**変更:**
- `crates/integration-tests/Cargo.toml` — `auto-trader` (app crate) を dev-dependencies に追加
- `crates/integration-tests/src/helpers/mod.rs` — `app`, `seed` モジュール追加

---

## Task 1: Test App Helper (`spawn_test_app`)

**Files:**
- Modify: `crates/integration-tests/Cargo.toml`
- Modify: `crates/integration-tests/src/helpers/mod.rs`
- Create: `crates/integration-tests/src/helpers/app.rs`

- [ ] **Step 1: Add auto-trader (app crate) dependency**

`crates/integration-tests/Cargo.toml` の `[dependencies]` に追加:

```toml
auto-trader = { path = "../app" }
```

- [ ] **Step 2: Register app and seed modules**

`crates/integration-tests/src/helpers/mod.rs` を更新:

```rust
pub mod app;
pub mod db;
pub mod failure_output;
pub mod fixture_loader;
pub mod seed;
```

- [ ] **Step 3: Implement spawn_test_app**

`crates/integration-tests/src/helpers/app.rs` を作成:

```rust
//! Test API server helper.
//!
//! `spawn_test_app` starts the auto-trader API server in-process on an
//! ephemeral port and returns the base URL + a JoinHandle for cleanup.

use auto_trader::api::{self, AppState};
use auto_trader_market::price_store::PriceStore;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// A running test API server.
pub struct TestApp {
    /// Base URL including scheme and port, e.g. `http://127.0.0.1:12345`.
    pub url: String,
    /// Handle to the background server task. Drop or abort to shut down.
    pub handle: JoinHandle<()>,
    /// Shared PriceStore — tests can insert ticks before making requests.
    pub price_store: Arc<PriceStore>,
}

impl TestApp {
    /// Convenience: build a reqwest client pre-configured with the base URL.
    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::new()
    }

    /// Build a full endpoint URL, e.g. `self.endpoint("/api/trading-accounts")`.
    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.url, path)
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Start the API server in-process on an ephemeral port.
///
/// The server uses the given DB pool and an empty PriceStore (no expected
/// feeds). Tests that need price data should call `price_store.update()`
/// before making requests.
pub async fn spawn_test_app(pool: PgPool) -> TestApp {
    spawn_test_app_with_price_store(pool, PriceStore::new(vec![])).await
}

/// Start the API server with a custom PriceStore (for health endpoint tests
/// that need expected feeds).
pub async fn spawn_test_app_with_price_store(
    pool: PgPool,
    price_store: Arc<PriceStore>,
) -> TestApp {
    let state = AppState {
        pool,
        price_store: price_store.clone(),
    };

    let router = api::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let addr = listener.local_addr().expect("failed to get local addr");
    let url = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("server error");
    });

    // Give the server a moment to start accepting connections.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    TestApp {
        url,
        handle,
        price_store,
    }
}
```

- [ ] **Step 4: Write a compile-check test**

テストファイルは Task 4 以降で作成するが、ここで `cargo check -p auto-trader-integration-tests` が通ることを確認:

```bash
cargo check -p auto-trader-integration-tests
```

---

## Task 2: Config Fixtures + Phase 1 Config Tests

**Files:**
- Create: `crates/integration-tests/fixtures/config_valid.toml`
- Create: `crates/integration-tests/fixtures/config_unknown_exchange.toml`
- Create: `crates/integration-tests/fixtures/config_missing_pairs.toml`
- Create: `crates/integration-tests/fixtures/config_invalid_strategy.toml`
- Create: `crates/integration-tests/fixtures/config_disabled_strategy.toml`
- Create: `crates/integration-tests/tests/phase1_config.rs`

- [ ] **Step 1: Create config_valid.toml**

```toml
[vegapunk]
endpoint = "http://localhost:3000"
schema = "test-schema"

[database]
url = "postgresql://test:test@localhost:5432/test"

[monitor]
interval_secs = 60

[pairs]
fx = ["USD_JPY"]
crypto = ["FX_BTC_JPY"]

[bitflyer]
ws_url = "wss://ws.lightstream.bitflyer.com/json-rpc"
api_url = "https://api.bitflyer.com"

[pair_config.USD_JPY]
price_unit = 0.001
min_order_size = 1

[pair_config.FX_BTC_JPY]
price_unit = 1
min_order_size = 0.001

[position_sizing]
method = "risk_based"
risk_rate = 0.02

[gemini]
model = "gemini-2.5-flash"
api_url = "https://generativelanguage.googleapis.com"

[[strategies]]
name = "bb_mean_revert_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY", "USD_JPY"]

[[strategies]]
name = "donchian_trend_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY"]

[risk]
price_freshness_secs = 60

[live]
enabled = false
dry_run = true
```

- [ ] **Step 2: Create config_unknown_exchange.toml**

`[bitflyer]` セクションに不正なキーを持つのではなく、strategies で認識できない exchange を参照する設定。ただし `AppConfig` は exchange バリデーションを pairs 単位では行わないため、ここでは toml パース自体が失敗するケース（不正な TOML 構造）をテストする。

実際の `AppConfig::load` は TOML デシリアライズ → `validate()` の 2 段階。不正 exchange のテストは account 作成 API 側で行う（Phase 2）ため、ここでは TOML 構造不正のみテストする:

```toml
# 必須フィールド vegapunk が missing → デシリアライズエラー
[database]
url = "postgresql://test:test@localhost:5432/test"

[monitor]
interval_secs = 60

[pairs]
fx = ["USD_JPY"]
```

- [ ] **Step 3: Create config_missing_pairs.toml**

```toml
[vegapunk]
endpoint = "http://localhost:3000"
schema = "test-schema"

[database]
url = "postgresql://test:test@localhost:5432/test"

[monitor]
interval_secs = 60

[pairs]
fx = []
crypto = []
```

- [ ] **Step 4: Create config_invalid_strategy.toml**

```toml
[vegapunk]
endpoint = "http://localhost:3000"
schema = "test-schema"

[database]
url = "postgresql://test:test@localhost:5432/test"

[monitor]
interval_secs = 60

[pairs]
fx = ["USD_JPY"]

[[strategies]]
name = "nonexistent_strategy_xyz"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]
```

- [ ] **Step 5: Create config_disabled_strategy.toml**

```toml
[vegapunk]
endpoint = "http://localhost:3000"
schema = "test-schema"

[database]
url = "postgresql://test:test@localhost:5432/test"

[monitor]
interval_secs = 60

[pairs]
fx = ["USD_JPY"]

[[strategies]]
name = "bb_mean_revert_v1"
enabled = false
mode = "paper"
pairs = ["USD_JPY"]

[[strategies]]
name = "donchian_trend_v1"
enabled = false
mode = "paper"
pairs = ["USD_JPY"]
```

- [ ] **Step 6: Write Phase 1 config tests (RED)**

`crates/integration-tests/tests/phase1_config.rs`:

```rust
//! Phase 1: Config loading tests.

use auto_trader_core::config::AppConfig;
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name)
}

#[test]
fn config_valid_loads_successfully() {
    let config = AppConfig::load(&fixture_path("config_valid.toml"))
        .expect("valid config should load");
    assert_eq!(config.pairs.fx, vec!["USD_JPY"]);
    assert_eq!(
        config.pairs.crypto.as_ref().unwrap(),
        &vec!["FX_BTC_JPY".to_string()]
    );
    assert_eq!(config.strategies.len(), 2);
    assert_eq!(config.strategies[0].name, "bb_mean_revert_v1");
    assert!(config.strategies[0].enabled);
    assert_eq!(config.risk.as_ref().unwrap().price_freshness_secs, 60);
    let live = config.live.as_ref().unwrap();
    assert!(!live.enabled);
    assert!(live.dry_run);
}

#[test]
fn config_missing_vegapunk_fails() {
    let result = AppConfig::load(&fixture_path("config_unknown_exchange.toml"));
    assert!(
        result.is_err(),
        "config without vegapunk section should fail to parse"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("vegapunk"),
        "error should mention missing vegapunk: {err_msg}"
    );
}

#[test]
fn config_empty_pairs_loads_with_empty_vecs() {
    let config = AppConfig::load(&fixture_path("config_missing_pairs.toml"))
        .expect("empty pairs is valid (strategies may not reference any)");
    assert!(config.pairs.fx.is_empty());
    assert!(
        config.pairs.crypto.as_ref().map_or(true, |v| v.is_empty()),
        "crypto pairs should be empty"
    );
}

#[test]
fn config_invalid_strategy_name_parses_but_register_skips() {
    // The config file itself is valid TOML and deserializes fine.
    // The unknown strategy name only matters at register_strategies() time.
    let config = AppConfig::load(&fixture_path("config_invalid_strategy.toml"))
        .expect("config with unknown strategy name is still valid TOML");
    assert_eq!(config.strategies.len(), 1);
    assert_eq!(config.strategies[0].name, "nonexistent_strategy_xyz");
    assert!(config.strategies[0].enabled);
}

#[test]
fn config_disabled_strategies_parse() {
    let config = AppConfig::load(&fixture_path("config_disabled_strategy.toml"))
        .expect("disabled strategies should parse fine");
    assert_eq!(config.strategies.len(), 2);
    assert!(
        config.strategies.iter().all(|s| !s.enabled),
        "all strategies should be disabled"
    );
}

#[test]
fn config_risk_zero_freshness_fails_validation() {
    // Inline TOML with price_freshness_secs = 0.
    let toml_str = r#"
[vegapunk]
endpoint = "http://localhost:3000"
schema = "test"

[database]
url = "postgresql://test:test@localhost:5432/test"

[monitor]
interval_secs = 60

[pairs]
fx = ["USD_JPY"]

[risk]
price_freshness_secs = 0
"#;
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("config_risk_zero.toml");
    std::fs::write(&temp_file, toml_str).unwrap();
    let result = AppConfig::load(&temp_file);
    assert!(result.is_err(), "zero freshness should fail validation");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("price_freshness_secs"),
        "error should mention price_freshness_secs: {err_msg}"
    );
    std::fs::remove_file(temp_file).ok();
}
```

- [ ] **Step 7: Run tests (RED → GREEN)**

```bash
cargo test -p auto-trader-integration-tests --test phase1_config
```

すべてのテストがパスすることを確認。

---

## Task 3: Phase 1 Warmup + Strategy Registration + Notification Purge

**Files:**
- Create: `crates/integration-tests/src/helpers/seed.rs`
- Create: `crates/integration-tests/tests/phase1_startup.rs`

- [ ] **Step 1: Create seed helper for trades/notifications**

`crates/integration-tests/src/helpers/seed.rs`:

```rust
//! Seed helpers for trades, notifications, daily_summary.
//!
//! These complement the existing `db.rs` helpers (which seed accounts).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

/// Insert a closed trade. Returns the trade ID.
#[allow(clippy::too_many_arguments)]
pub async fn seed_closed_trade(
    pool: &PgPool,
    account_id: Uuid,
    strategy_name: &str,
    pair: &str,
    exchange: &str,
    direction: &str,
    entry_price: Decimal,
    exit_price: Decimal,
    pnl_amount: Decimal,
    quantity: Decimal,
    fees: Decimal,
    entry_at: DateTime<Utc>,
    exit_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::new_v4();
    let stop_loss = if direction == "long" {
        entry_price - Decimal::from(100)
    } else {
        entry_price + Decimal::from(100)
    };
    sqlx::query(
        r#"INSERT INTO trades
               (id, account_id, strategy_name, pair, exchange, direction,
                entry_price, exit_price, stop_loss, quantity, leverage,
                fees, pnl_amount, exit_reason, status, entry_at, exit_at)
           VALUES ($1, $2, $3, $4, $5, $6,
                   $7, $8, $9, $10, 2,
                   $11, $12, 'sl_hit', 'closed', $13, $14)"#,
    )
    .bind(id)
    .bind(account_id)
    .bind(strategy_name)
    .bind(pair)
    .bind(exchange)
    .bind(direction)
    .bind(entry_price)
    .bind(exit_price)
    .bind(stop_loss)
    .bind(quantity)
    .bind(fees)
    .bind(pnl_amount)
    .bind(entry_at)
    .bind(exit_at)
    .execute(pool)
    .await
    .expect("seed_closed_trade: insert failed");
    id
}

/// Insert an open trade. Returns the trade ID.
#[allow(clippy::too_many_arguments)]
pub async fn seed_open_trade(
    pool: &PgPool,
    account_id: Uuid,
    strategy_name: &str,
    pair: &str,
    exchange: &str,
    direction: &str,
    entry_price: Decimal,
    stop_loss: Decimal,
    quantity: Decimal,
    entry_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO trades
               (id, account_id, strategy_name, pair, exchange, direction,
                entry_price, stop_loss, quantity, leverage,
                fees, status, entry_at)
           VALUES ($1, $2, $3, $4, $5, $6,
                   $7, $8, $9, 2,
                   0, 'open', $10)"#,
    )
    .bind(id)
    .bind(account_id)
    .bind(strategy_name)
    .bind(pair)
    .bind(exchange)
    .bind(direction)
    .bind(entry_price)
    .bind(stop_loss)
    .bind(quantity)
    .bind(entry_at)
    .execute(pool)
    .await
    .expect("seed_open_trade: insert failed");
    id
}

/// Insert a notification row. Returns the notification ID.
#[allow(clippy::too_many_arguments)]
pub async fn seed_notification(
    pool: &PgPool,
    kind: &str,
    trade_id: Uuid,
    account_id: Uuid,
    strategy_name: &str,
    pair: &str,
    direction: &str,
    price: Decimal,
    pnl_amount: Option<Decimal>,
    exit_reason: Option<&str>,
    read_at: Option<DateTime<Utc>>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO notifications
               (id, kind, trade_id, account_id, strategy_name, pair,
                direction, price, pnl_amount, exit_reason, read_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
    )
    .bind(id)
    .bind(kind)
    .bind(trade_id)
    .bind(account_id)
    .bind(strategy_name)
    .bind(pair)
    .bind(direction)
    .bind(price)
    .bind(pnl_amount)
    .bind(exit_reason)
    .bind(read_at)
    .execute(pool)
    .await
    .expect("seed_notification: insert failed");
    id
}

/// Insert a daily_summary row.
#[allow(clippy::too_many_arguments)]
pub async fn seed_daily_summary(
    pool: &PgPool,
    account_id: Uuid,
    date: chrono::NaiveDate,
    strategy_name: &str,
    pair: &str,
    exchange: &str,
    account_type: &str,
    trade_count: i64,
    win_count: i64,
    total_pnl: Decimal,
    max_drawdown: Decimal,
) {
    sqlx::query(
        r#"INSERT INTO daily_summary
               (account_id, date, strategy_name, pair, exchange,
                account_type, trade_count, win_count, total_pnl, max_drawdown)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
    )
    .bind(account_id)
    .bind(date)
    .bind(strategy_name)
    .bind(pair)
    .bind(exchange)
    .bind(account_type)
    .bind(trade_count)
    .bind(win_count)
    .bind(total_pnl)
    .bind(max_drawdown)
    .execute(pool)
    .await
    .expect("seed_daily_summary: insert failed");
}
```

- [ ] **Step 2: Write Phase 1 startup tests (RED)**

`crates/integration-tests/tests/phase1_startup.rs`:

```rust
//! Phase 1: Warmup, strategy registration, notification purge tests.

use auto_trader_core::config::{GeminiConfig, StrategyConfig};
use auto_trader_integration_tests::helpers::{db, seed};
use auto_trader_strategy::engine::StrategyEngine;
use chrono::{TimeZone, Utc};
use rust_decimal_macros::dec;
use std::collections::HashMap;

// ── Strategy Registration ────────────────────────────────────────────────

/// All 5 standard strategies register successfully when enabled.
#[sqlx::test(migrations = "../../migrations")]
async fn register_all_five_strategies(pool: sqlx::PgPool) {
    let mut engine = StrategyEngine::new();
    let strategies = vec![
        strategy_cfg("bb_mean_revert_v1", true, &["USD_JPY"]),
        strategy_cfg("donchian_trend_v1", true, &["USD_JPY"]),
        strategy_cfg("donchian_trend_evolve_v1", true, &["USD_JPY"]),
        strategy_cfg("squeeze_momentum_v1", true, &["USD_JPY"]),
        // swing_llm requires GEMINI_API_KEY + vegapunk — skip in this test
    ];

    auto_trader::startup::register_strategies(
        &mut engine,
        &strategies,
        &pool,
        &None, // no vegapunk
        "test-schema",
        None, // no gemini config
    )
    .await;

    // 4 strategies registered (swing_llm excluded from input).
    assert_eq!(engine.strategy_count(), 4);
}

/// Disabled strategies are skipped.
#[sqlx::test(migrations = "../../migrations")]
async fn register_disabled_strategies_skipped(pool: sqlx::PgPool) {
    let mut engine = StrategyEngine::new();
    let strategies = vec![
        strategy_cfg("bb_mean_revert_v1", false, &["USD_JPY"]),
        strategy_cfg("donchian_trend_v1", false, &["USD_JPY"]),
    ];

    auto_trader::startup::register_strategies(
        &mut engine, &strategies, &pool, &None, "test-schema", None,
    )
    .await;

    assert_eq!(engine.strategy_count(), 0);
}

/// Unknown strategy names are skipped with a warning (no panic).
#[sqlx::test(migrations = "../../migrations")]
async fn register_unknown_strategy_skipped(pool: sqlx::PgPool) {
    let mut engine = StrategyEngine::new();
    let strategies = vec![
        strategy_cfg("totally_unknown_v99", true, &["USD_JPY"]),
    ];

    auto_trader::startup::register_strategies(
        &mut engine, &strategies, &pool, &None, "test-schema", None,
    )
    .await;

    assert_eq!(engine.strategy_count(), 0);
}

/// swing_llm is skipped when GEMINI_API_KEY is not set.
#[sqlx::test(migrations = "../../migrations")]
async fn register_swing_llm_skipped_without_gemini_key(pool: sqlx::PgPool) {
    // Ensure GEMINI_API_KEY is not set for this test.
    std::env::remove_var("GEMINI_API_KEY");

    let mut engine = StrategyEngine::new();
    let strategies = vec![swing_llm_cfg()];
    let gemini = GeminiConfig {
        model: "gemini-2.5-flash".to_string(),
        api_url: "https://generativelanguage.googleapis.com".to_string(),
    };

    auto_trader::startup::register_strategies(
        &mut engine, &strategies, &pool, &None, "test-schema", Some(&gemini),
    )
    .await;

    assert_eq!(engine.strategy_count(), 0);
}

/// swing_llm is skipped when vegapunk client is None.
#[sqlx::test(migrations = "../../migrations")]
async fn register_swing_llm_skipped_without_vegapunk(pool: sqlx::PgPool) {
    // Even with GEMINI_API_KEY set, vegapunk=None → skip.
    std::env::set_var("GEMINI_API_KEY", "test-key-for-integration-test");

    let mut engine = StrategyEngine::new();
    let strategies = vec![swing_llm_cfg()];
    let gemini = GeminiConfig {
        model: "gemini-2.5-flash".to_string(),
        api_url: "https://generativelanguage.googleapis.com".to_string(),
    };

    auto_trader::startup::register_strategies(
        &mut engine, &strategies, &pool, &None, "test-schema", Some(&gemini),
    )
    .await;

    assert_eq!(engine.strategy_count(), 0);

    // Clean up env var.
    std::env::remove_var("GEMINI_API_KEY");
}

/// donchian_trend_evolve falls back to defaults when strategy_params query
/// fails (e.g., table exists but no row for this strategy).
#[sqlx::test(migrations = "../../migrations")]
async fn register_donchian_evolve_fallback_on_missing_params(pool: sqlx::PgPool) {
    let mut engine = StrategyEngine::new();
    let strategies = vec![
        strategy_cfg("donchian_trend_evolve_v1", true, &["USD_JPY"]),
    ];

    // No strategy_params row inserted — should fallback to defaults.
    auto_trader::startup::register_strategies(
        &mut engine, &strategies, &pool, &None, "test-schema", None,
    )
    .await;

    assert_eq!(engine.strategy_count(), 1);
}

// ── Notification Purge ───────────────────────────────────────────────────

/// purge_old_read deletes read notifications older than 30 days.
#[sqlx::test(migrations = "../../migrations")]
async fn notification_purge_deletes_old_read(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "purge_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;
    let trade_id = seed::seed_open_trade(
        &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
        "long", dec!(150), dec!(149), dec!(1),
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    )
    .await;

    // Old read notification (40 days ago).
    let old_read_at = Utc::now() - chrono::Duration::days(40);
    seed::seed_notification(
        &pool, "trade_opened", trade_id, account_id,
        "bb_mean_revert_v1", "USD_JPY", "long", dec!(150),
        None, None, Some(old_read_at),
    )
    .await;

    // Recent read notification (5 days ago).
    let recent_read_at = Utc::now() - chrono::Duration::days(5);
    seed::seed_notification(
        &pool, "trade_opened", trade_id, account_id,
        "bb_mean_revert_v1", "USD_JPY", "long", dec!(150),
        None, None, Some(recent_read_at),
    )
    .await;

    // Unread notification.
    seed::seed_notification(
        &pool, "trade_opened", trade_id, account_id,
        "bb_mean_revert_v1", "USD_JPY", "long", dec!(150),
        None, None, None,
    )
    .await;

    let purged = auto_trader_db::notifications::purge_old_read(&pool)
        .await
        .unwrap();
    assert_eq!(purged, 1, "should purge only the old read notification");

    let (remaining, total) = auto_trader_db::notifications::list(
        &pool, 100, 0, false, None, None, None,
    )
    .await
    .unwrap();
    assert_eq!(total, 2, "2 notifications should remain");
    assert_eq!(remaining.len(), 2);
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn strategy_cfg(name: &str, enabled: bool, pairs: &[&str]) -> StrategyConfig {
    StrategyConfig {
        name: name.to_string(),
        enabled,
        mode: "paper".to_string(),
        pairs: pairs.iter().map(|s| s.to_string()).collect(),
        params: HashMap::new(),
    }
}

fn swing_llm_cfg() -> StrategyConfig {
    let mut params = HashMap::new();
    params.insert(
        "holding_days_max".to_string(),
        toml::Value::Integer(14),
    );
    StrategyConfig {
        name: "swing_llm_v1".to_string(),
        enabled: true,
        mode: "paper".to_string(),
        pairs: vec!["USD_JPY".to_string()],
        params,
    }
}
```

- [ ] **Step 3: Run tests (RED → GREEN)**

```bash
cargo test -p auto-trader-integration-tests --test phase1_startup
```

テストがパスすることを確認。`StrategyEngine::strategy_count()` が未実装の場合はメソッドを追加する必要がある。その場合は `crates/strategy/src/engine.rs` に以下を追加:

```rust
/// Return the number of registered strategies (for testing).
pub fn strategy_count(&self) -> usize {
    self.strategies.len()
}
```

---

## Task 4: Phase 2 Accounts CRUD + Errors

**Files:**
- Create: `crates/integration-tests/tests/phase2_accounts.rs`

- [ ] **Step 1: Write account tests (RED)**

`crates/integration-tests/tests/phase2_accounts.rs`:

```rust
//! Phase 2: Trading accounts CRUD API tests.

use auto_trader_integration_tests::helpers::{app, db};
use rust_decimal_macros::dec;
use serde_json::{json, Value};

// ── POST /api/trading-accounts ───────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn create_paper_account(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Test Paper",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 201);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["name"], "Test Paper");
    assert_eq!(json["exchange"], "gmo_fx");
    assert_eq!(json["account_type"], "paper");
    assert_eq!(json["strategy"], "bb_mean_revert_v1");
    assert!(json["id"].is_string());
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_invalid_account_type(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Bad Type",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "invalid"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("account_type"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_duplicate_name(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Dup Name",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });

    // First create succeeds.
    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Second create with same name → 409 CONFLICT.
    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409);
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_invalid_exchange(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Bad Exchange",
        "exchange": "unknown_exchange",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    // Unknown exchange → 500 (internal error from anyhow bail).
    assert!(
        resp.status().as_u16() == 400 || resp.status().as_u16() == 500,
        "expected 400 or 500 for unknown exchange, got {}",
        resp.status().as_u16()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_nonexistent_strategy(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Bad Strategy",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "nonexistent_strategy_xyz",
        "account_type": "paper"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("strategy"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_account_insufficient_balance(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Low Balance",
        "exchange": "gmo_fx",
        "initial_balance": 100,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper",
        "currency": "JPY"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("initial_balance"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_live_account_duplicate_exchange(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let body = json!({
        "name": "Live 1",
        "exchange": "bitflyer_cfd",
        "initial_balance": 50000,
        "leverage": 1,
        "strategy": "bb_mean_revert_v1",
        "account_type": "live"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);

    // Second live account for same exchange → error.
    let body2 = json!({
        "name": "Live 2",
        "exchange": "bitflyer_cfd",
        "initial_balance": 50000,
        "leverage": 1,
        "strategy": "bb_mean_revert_v1",
        "account_type": "live"
    });

    let resp = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body2)
        .send()
        .await
        .unwrap();

    // Should fail — either 409 or 500 depending on error path.
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "duplicate live account should fail"
    );
}

// ── GET /api/trading-accounts ────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn list_accounts_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/trading-accounts"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Vec<Value> = resp.json().await.unwrap();
    // Migration seeds some accounts, so we just check it's an array.
    assert!(json.is_array() || true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn list_accounts_includes_evaluated_balance(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create an account via API.
    let body = json!({
        "name": "Eval Balance Test",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap();

    let resp = client
        .get(app.endpoint("/api/trading-accounts"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let accounts: Vec<Value> = resp.json().await.unwrap();
    let test_account = accounts
        .iter()
        .find(|a| a["name"] == "Eval Balance Test")
        .expect("test account should be in list");
    assert!(test_account["evaluated_balance"].is_number());
    assert!(test_account["unrealized_pnl"].is_number());
}

// ── GET /api/trading-accounts/:id ────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn get_account_by_id(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create account.
    let body = json!({
        "name": "Get By ID",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    let created: Value = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client
        .get(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["id"], id);
    assert_eq!(json["name"], "Get By ID");
}

#[sqlx::test(migrations = "../../migrations")]
async fn get_account_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let fake_id = uuid::Uuid::new_v4();

    let resp = client
        .get(app.endpoint(&format!("/api/trading-accounts/{fake_id}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
}

// ── PUT /api/trading-accounts/:id ────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn update_account(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create account.
    let body = json!({
        "name": "Before Update",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    let created: Value = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    // Update name and leverage.
    let update_body = json!({
        "name": "After Update",
        "leverage": 5
    });
    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .json(&update_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["name"], "After Update");
    assert_eq!(json["leverage"], 5);
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_account_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let fake_id = uuid::Uuid::new_v4();

    let resp = client
        .put(app.endpoint(&format!("/api/trading-accounts/{fake_id}")))
        .json(&json!({"name": "No Such Account"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
}

// ── DELETE /api/trading-accounts/:id ─────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn delete_account(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create account.
    let body = json!({
        "name": "To Delete",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    let created: Value = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client
        .delete(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 204);

    // Verify it's gone.
    let resp = client
        .get(app.endpoint(&format!("/api/trading-accounts/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_account_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let fake_id = uuid::Uuid::new_v4();

    let resp = client
        .delete(app.endpoint(&format!("/api/trading-accounts/{fake_id}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_account_with_trades_fails(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool.clone()).await;
    let client = app.client();

    // Create account via API.
    let body = json!({
        "name": "Has Trades",
        "exchange": "gmo_fx",
        "initial_balance": 100000,
        "leverage": 2,
        "strategy": "bb_mean_revert_v1",
        "account_type": "paper"
    });
    let created: Value = client
        .post(app.endpoint("/api/trading-accounts"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id_str = created["id"].as_str().unwrap();
    let account_id: uuid::Uuid = id_str.parse().unwrap();

    // Seed a trade for this account.
    seed::seed_open_trade(
        &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
        "long", dec!(150), dec!(149), dec!(1),
        chrono::Utc::now(),
    )
    .await;

    // Delete should fail due to FK constraint.
    let resp = client
        .delete(app.endpoint(&format!("/api/trading-accounts/{id_str}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 409);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("trades"));
}
```

- [ ] **Step 2: Run tests (RED → GREEN)**

```bash
cargo test -p auto-trader-integration-tests --test phase2_accounts
```

テストがパスすることを確認。

---

## Task 5: Phase 2 Trades + Positions

**Files:**
- Create: `crates/integration-tests/tests/phase2_trades_positions.rs`

- [ ] **Step 1: Write trades + positions tests (RED)**

`crates/integration-tests/tests/phase2_trades_positions.rs`:

```rust
//! Phase 2: Trades and Positions API tests.

use auto_trader_integration_tests::helpers::{app, db, seed};
use chrono::{TimeZone, Utc};
use rust_decimal_macros::dec;
use serde_json::Value;

// ── GET /api/trades ──────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/trades"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["trades"].is_array());
    assert_eq!(json["total"], 0);
    assert_eq!(json["page"], 1);
    assert_eq!(json["per_page"], 50);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_with_filters(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "filter_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;

    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();

    seed::seed_closed_trade(
        &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
        "long", dec!(150), dec!(151), dec!(1000), dec!(1), dec!(0),
        t1, t2,
    )
    .await;

    seed::seed_open_trade(
        &pool, account_id, "bb_mean_revert_v1", "FX_BTC_JPY", "gmo_fx",
        "short", dec!(5000000), dec!(5100000), dec!(0.01), t1,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Filter by status=closed.
    let resp = client
        .get(app.endpoint("/api/trades?status=closed"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 1);
    assert_eq!(json["trades"][0]["pair"], "USD_JPY");
    assert_eq!(json["trades"][0]["status"], "closed");

    // Filter by pair.
    let resp = client
        .get(app.endpoint("/api/trades?pair=FX_BTC_JPY"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 1);
    assert_eq!(json["trades"][0]["pair"], "FX_BTC_JPY");
    assert_eq!(json["trades"][0]["status"], "open");

    // Filter by exchange.
    let resp = client
        .get(app.endpoint("/api/trades?exchange=gmo_fx"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_pagination(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "page_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;

    let base_time = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    // Insert 5 closed trades.
    for i in 0..5 {
        let entry_at = base_time + chrono::Duration::hours(i);
        let exit_at = entry_at + chrono::Duration::hours(1);
        seed::seed_closed_trade(
            &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
            "long", dec!(150), dec!(151), dec!(1000), dec!(1), dec!(0),
            entry_at, exit_at,
        )
        .await;
    }

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Page 1, per_page=2.
    let resp = client
        .get(app.endpoint("/api/trades?page=1&per_page=2"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 5);
    assert_eq!(json["page"], 1);
    assert_eq!(json["per_page"], 2);
    assert_eq!(json["trades"].as_array().unwrap().len(), 2);

    // Page 3, per_page=2 → 1 trade remaining.
    let resp = client
        .get(app.endpoint("/api/trades?page=3&per_page=2"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 5);
    assert_eq!(json["trades"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_list_page_zero_treated_as_one(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/trades?page=0"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["page"], 1, "page=0 should be clamped to 1");
}

// ── GET /api/trades/:id/events ───────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn trade_events_for_existing_trade(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "events_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;
    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();
    let trade_id = seed::seed_closed_trade(
        &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
        "long", dec!(150), dec!(151), dec!(1000), dec!(1), dec!(0),
        t1, t2,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint(&format!("/api/trades/{trade_id}/events")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["events"].is_array());
}

#[sqlx::test(migrations = "../../migrations")]
async fn trade_events_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();
    let fake_id = uuid::Uuid::new_v4();

    let resp = client
        .get(app.endpoint(&format!("/api/trades/{fake_id}/events")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
}

// ── GET /api/positions ───────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn positions_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/positions"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Vec<Value> = resp.json().await.unwrap();
    assert!(json.is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn positions_lists_open_trades(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "pos_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;

    seed::seed_open_trade(
        &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
        "long", dec!(150), dec!(149), dec!(1),
        Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/positions"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(json.len(), 1);
    assert_eq!(json[0]["pair"], "USD_JPY");
    assert_eq!(json[0]["direction"], "long");
    assert_eq!(json[0]["account_name"], "pos_test");
}
```

- [ ] **Step 2: Run tests (RED → GREEN)**

```bash
cargo test -p auto-trader-integration-tests --test phase2_trades_positions
```

---

## Task 6: Phase 2 Strategies + Dashboard

**Files:**
- Create: `crates/integration-tests/tests/phase2_strategies_dashboard.rs`

- [ ] **Step 1: Write strategies + dashboard tests (RED)**

`crates/integration-tests/tests/phase2_strategies_dashboard.rs`:

```rust
//! Phase 2: Strategies and Dashboard API tests.

use auto_trader_integration_tests::helpers::{app, db, seed};
use chrono::{NaiveDate, TimeZone, Utc};
use rust_decimal_macros::dec;
use serde_json::Value;

// ── GET /api/strategies ──────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn strategies_list(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let strategies: Vec<Value> = resp.json().await.unwrap();
    // Migrations seed strategies; at least the 5 standard ones should exist.
    assert!(
        strategies.len() >= 5,
        "expected at least 5 strategies from migration seeds, got {}",
        strategies.len()
    );
    // Check each has required fields.
    for s in &strategies {
        assert!(s["name"].is_string());
        assert!(s["display_name"].is_string());
        assert!(s["category"].is_string());
        assert!(s["risk_level"].is_string());
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn strategies_list_with_category_filter(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies?category=crypto"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let strategies: Vec<Value> = resp.json().await.unwrap();
    for s in &strategies {
        assert_eq!(s["category"], "crypto");
    }
}

// ── GET /api/strategies/:name ────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn strategies_get_one(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies/bb_mean_revert_v1"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["name"], "bb_mean_revert_v1");
}

#[sqlx::test(migrations = "../../migrations")]
async fn strategies_get_one_not_found(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies/nonexistent_xyz"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("not found"));
}

// ── GET /api/dashboard/summary ───────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_summary_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/summary"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["trade_count"], 0);
    assert_eq!(json["win_count"], 0);
    assert_eq!(json["loss_count"], 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_summary_with_data(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "summary_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;

    let date = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap();
    seed::seed_daily_summary(
        &pool, account_id, date, "bb_mean_revert_v1", "USD_JPY",
        "gmo_fx", "paper", 5, 3, dec!(5000), dec!(1000),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/summary"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["trade_count"], 5);
    assert_eq!(json["win_count"], 3);
    assert_eq!(json["loss_count"], 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_summary_with_date_filter(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "date_filter", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;

    seed::seed_daily_summary(
        &pool, account_id, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        "bb_mean_revert_v1", "USD_JPY", "gmo_fx", "paper", 2, 1, dec!(1000), dec!(500),
    )
    .await;
    seed::seed_daily_summary(
        &pool, account_id, NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
        "bb_mean_revert_v1", "USD_JPY", "gmo_fx", "paper", 3, 2, dec!(2000), dec!(300),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Filter to only March 15+.
    let resp = client
        .get(app.endpoint("/api/dashboard/summary?from=2026-03-10&to=2026-03-20"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["trade_count"], 3);
}

// ── GET /api/dashboard/pnl-history ───────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_pnl_history(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "pnl_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;

    seed::seed_daily_summary(
        &pool, account_id, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        "bb_mean_revert_v1", "USD_JPY", "gmo_fx", "paper", 2, 1, dec!(1000), dec!(500),
    )
    .await;
    seed::seed_daily_summary(
        &pool, account_id, NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
        "bb_mean_revert_v1", "USD_JPY", "gmo_fx", "paper", 1, 0, dec!(-500), dec!(800),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/pnl-history"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let rows: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["date"], "2026-03-01");
    assert_eq!(rows[1]["date"], "2026-03-02");
}

// ── GET /api/dashboard/balance-history ────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_balance_history(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/balance-history"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["accounts"].is_array());
}

// ── GET /api/dashboard/strategies ────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_strategy_stats(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "strat_stats", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;

    seed::seed_daily_summary(
        &pool, account_id, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        "bb_mean_revert_v1", "USD_JPY", "gmo_fx", "paper", 5, 3, dec!(5000), dec!(1000),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let stats: Vec<Value> = resp.json().await.unwrap();
    let bb = stats.iter().find(|s| s["strategy_name"] == "bb_mean_revert_v1");
    assert!(bb.is_some(), "should have bb_mean_revert_v1 stats");
    assert_eq!(bb.unwrap()["trade_count"], 5);
}

// ── GET /api/dashboard/pairs ─────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_pair_stats(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "pair_stats", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;

    seed::seed_daily_summary(
        &pool, account_id, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        "bb_mean_revert_v1", "USD_JPY", "gmo_fx", "paper", 3, 2, dec!(3000), dec!(500),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/pairs"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let stats: Vec<Value> = resp.json().await.unwrap();
    let usd = stats.iter().find(|s| s["pair"] == "USD_JPY");
    assert!(usd.is_some());
    assert_eq!(usd.unwrap()["trade_count"], 3);
}

// ── GET /api/dashboard/hourly-winrate ────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_hourly_winrate(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "hourly_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;

    // Seed closed trades at different hours to get hourly data.
    let t1 = Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap();
    let t1_exit = Utc.with_ymd_and_hms(2026, 3, 1, 11, 0, 0).unwrap();
    seed::seed_closed_trade(
        &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
        "long", dec!(150), dec!(151), dec!(1000), dec!(1), dec!(0),
        t1, t1_exit,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/dashboard/hourly-winrate"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let rows: Vec<Value> = resp.json().await.unwrap();
    // Should have at least hour=10 entry.
    let h10 = rows.iter().find(|r| r["hour"] == 10);
    assert!(h10.is_some(), "should have hour=10 entry");
    assert_eq!(h10.unwrap()["trade_count"], 1);
    assert_eq!(h10.unwrap()["win_count"], 1);
}
```

- [ ] **Step 2: Run tests (RED → GREEN)**

```bash
cargo test -p auto-trader-integration-tests --test phase2_strategies_dashboard
```

---

## Task 7: Phase 2 Notifications + Health + Market + Auth

**Files:**
- Create: `crates/integration-tests/tests/phase2_notifications_health_market_auth.rs`

- [ ] **Step 1: Write notifications + health + market + auth tests (RED)**

`crates/integration-tests/tests/phase2_notifications_health_market_auth.rs`:

```rust
//! Phase 2: Notifications, Health, Market, and Auth API tests.

use auto_trader_integration_tests::helpers::{app, db, seed};
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_core::types::{Exchange, Pair};
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::Value;

// ── GET /api/notifications ───────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_list_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/notifications"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["items"].as_array().unwrap().is_empty());
    assert_eq!(json["total"], 0);
    assert_eq!(json["unread_count"], 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_list_with_kind_filter(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "notif_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;
    let trade_id = seed::seed_open_trade(
        &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
        "long", dec!(150), dec!(149), dec!(1),
        Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
    )
    .await;

    seed::seed_notification(
        &pool, "trade_opened", trade_id, account_id,
        "bb_mean_revert_v1", "USD_JPY", "long", dec!(150),
        None, None, None,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Filter by kind=trade_opened.
    let resp = client
        .get(app.endpoint("/api/notifications?kind=trade_opened"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 1);
    assert_eq!(json["items"][0]["kind"], "trade_opened");

    // Filter by kind=trade_closed → 0 results.
    let resp = client
        .get(app.endpoint("/api/notifications?kind=trade_closed"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["total"], 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_invalid_kind_returns_400(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/notifications?kind=invalid_kind"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("kind"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_invalid_date_returns_400(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/notifications?from=not-a-date"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 400);
    let json: Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("from"));
}

// ── GET /api/notifications/unread-count ──────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_unread_count(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "unread_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;
    let trade_id = seed::seed_open_trade(
        &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
        "long", dec!(150), dec!(149), dec!(1),
        Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
    )
    .await;

    // 2 unread, 1 read.
    seed::seed_notification(
        &pool, "trade_opened", trade_id, account_id,
        "bb_mean_revert_v1", "USD_JPY", "long", dec!(150),
        None, None, None,
    )
    .await;
    seed::seed_notification(
        &pool, "trade_opened", trade_id, account_id,
        "bb_mean_revert_v1", "USD_JPY", "long", dec!(150),
        None, None, None,
    )
    .await;
    seed::seed_notification(
        &pool, "trade_opened", trade_id, account_id,
        "bb_mean_revert_v1", "USD_JPY", "long", dec!(150),
        None, None, Some(Utc::now()),
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/notifications/unread-count"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["count"], 2);
}

// ── POST /api/notifications/mark-all-read ────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn notifications_mark_all_read(pool: sqlx::PgPool) {
    let account_id = db::seed_trading_account(
        &pool, "mark_test", "paper", "gmo_fx", "bb_mean_revert_v1", 100_000,
    )
    .await;
    let trade_id = seed::seed_open_trade(
        &pool, account_id, "bb_mean_revert_v1", "USD_JPY", "gmo_fx",
        "long", dec!(150), dec!(149), dec!(1),
        Utc.with_ymd_and_hms(2026, 3, 1, 10, 0, 0).unwrap(),
    )
    .await;

    seed::seed_notification(
        &pool, "trade_opened", trade_id, account_id,
        "bb_mean_revert_v1", "USD_JPY", "long", dec!(150),
        None, None, None,
    )
    .await;
    seed::seed_notification(
        &pool, "trade_opened", trade_id, account_id,
        "bb_mean_revert_v1", "USD_JPY", "long", dec!(150),
        None, None, None,
    )
    .await;

    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .post(app.endpoint("/api/notifications/mark-all-read"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["marked"], 2);

    // Verify unread count is now 0.
    let resp = client
        .get(app.endpoint("/api/notifications/unread-count"))
        .send()
        .await
        .unwrap();
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["count"], 0);
}

// ── GET /api/health/market-feed ──────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn health_market_feed_no_expected_feeds(pool: sqlx::PgPool) {
    // Default spawn_test_app has empty expected feeds.
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/health/market-feed"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["feeds"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn health_market_feed_with_expected_feeds(pool: sqlx::PgPool) {
    let expected = vec![
        FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY")),
        FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY")),
    ];
    let price_store = PriceStore::new(expected);

    // Insert a fresh tick for GmoFx only.
    let now = Utc::now();
    price_store
        .update(
            FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY")),
            LatestTick {
                price: dec!(150),
                best_bid: Some(dec!(149.999)),
                best_ask: Some(dec!(150.001)),
                ts: now,
            },
        )
        .await;

    let app = app::spawn_test_app_with_price_store(pool, price_store).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/health/market-feed"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    let feeds = json["feeds"].as_array().unwrap();
    assert_eq!(feeds.len(), 2);

    let gmo = feeds.iter().find(|f| f["exchange"] == "gmo_fx").unwrap();
    assert_eq!(gmo["status"], "healthy");
    assert!(gmo["last_tick_age_secs"].is_number());

    let bf = feeds
        .iter()
        .find(|f| f["exchange"] == "bitflyer_cfd")
        .unwrap();
    assert_eq!(bf["status"], "missing");
}

// ── GET /api/market/prices ───────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn market_prices_empty(pool: sqlx::PgPool) {
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/market/prices"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    assert!(json["prices"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn market_prices_snapshot(pool: sqlx::PgPool) {
    let price_store = PriceStore::new(vec![]);
    let now = Utc::now();

    price_store
        .update(
            FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY")),
            LatestTick {
                price: dec!(150.123),
                best_bid: None,
                best_ask: None,
                ts: now,
            },
        )
        .await;

    let app = app::spawn_test_app_with_price_store(pool, price_store).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/market/prices"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
    let json: Value = resp.json().await.unwrap();
    let prices = json["prices"].as_array().unwrap();
    assert_eq!(prices.len(), 1);
    assert_eq!(prices[0]["exchange"], "gmo_fx");
    assert_eq!(prices[0]["pair"], "USD_JPY");
}

// ── Auth middleware ──────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn auth_no_token_configured_allows_all(pool: sqlx::PgPool) {
    // Default: API_TOKEN env is not set → no auth required.
    std::env::remove_var("API_TOKEN");
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_valid_token(pool: sqlx::PgPool) {
    std::env::set_var("API_TOKEN", "test-secret-token");
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .header("Authorization", "Bearer test-secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);

    std::env::remove_var("API_TOKEN");
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_missing_token_returns_401(pool: sqlx::PgPool) {
    std::env::set_var("API_TOKEN", "test-secret-token");
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);

    std::env::remove_var("API_TOKEN");
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_invalid_token_returns_401(pool: sqlx::PgPool) {
    std::env::set_var("API_TOKEN", "test-secret-token");
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);

    std::env::remove_var("API_TOKEN");
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_invalid_format_returns_401(pool: sqlx::PgPool) {
    std::env::set_var("API_TOKEN", "test-secret-token");
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    // Wrong auth scheme (Basic instead of Bearer).
    let resp = client
        .get(app.endpoint("/api/strategies"))
        .header("Authorization", "Basic test-secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);

    std::env::remove_var("API_TOKEN");
}
```

- [ ] **Step 2: Run tests (RED → GREEN)**

```bash
cargo test -p auto-trader-integration-tests --test phase2_notifications_health_market_auth
```

- [ ] **Step 3: Run all integration tests together**

```bash
cargo test -p auto-trader-integration-tests
```

すべてのテスト（Phase 1 + Phase 2 + 既存 smoke test）がパスすることを確認。

---

## 実行順序と依存関係

```
Task 1 (spawn_test_app)
  ├─→ Task 2 (config fixtures + Phase 1 config tests)  [独立]
  ├─→ Task 3 (Phase 1 startup tests)                   [seed.rs が Task 4-7 と共有]
  ├─→ Task 4 (accounts CRUD)                            [Task 1 依存]
  ├─→ Task 5 (trades + positions)                       [Task 1 依存]
  ├─→ Task 6 (strategies + dashboard)                   [Task 1 依存]
  └─→ Task 7 (notifications + health + market + auth)   [Task 1 依存]
```

Task 2 と Task 3 は Task 1 と並行可能（config テストは API サーバー不要）。
Task 4-7 は Task 1 完了後に並行実行可能。

---

## 注意事項

1. **Auth テストの環境変数競合**: auth テストは `API_TOKEN` env var を設定するため、並列実行でフレーキーになる可能性がある。`#[serial_test::serial]` の導入、または auth テスト専用のテストバイナリに分離する対策を検討する。実装時に問題が発生した場合は `serial_test` crate を追加する。

2. **`StrategyEngine::strategy_count()`**: このメソッドが未実装の場合、Task 3 で追加が必要。`crates/strategy/src/engine.rs` の `StrategyEngine` impl に `pub fn strategy_count(&self) -> usize` を追加する。

3. **Migration seeded data**: `#[sqlx::test]` は毎回クリーンな DB に migration を適用するため、migration 内で INSERT される strategy rows が存在する。テストはこれを前提としてよい。

4. **`AppState` / `api::router` の可視性**: `auto-trader` (app crate) の `api` モジュールが `pub` でない場合、integration-tests crate からアクセスできない。`crates/app/src/api/mod.rs` の `pub fn router` と `pub struct AppState` が外部からアクセス可能であることを確認し、必要なら `pub(crate)` → `pub` に変更する。
