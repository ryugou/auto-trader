# Plan 1: Test Infrastructure + Mock Implementations

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** integration-tests crate を新設し、DB セットアップ / フィクスチャローダー / 全 7 モック / テスト失敗出力フォーマットを実装する。最後に smoke test で基盤全体が動くことを確認する。

**Architecture:** `crates/integration-tests/` に独立テストクレートを配置。モック群は `src/mocks/` に集約し、Phase 2-4 の全テストが再利用できる形にする。DB は `#[sqlx::test]` を使って migration 自動適用 + テストごとの分離を実現。

**Tech Stack:** Rust (workspace edition 2024), sqlx (PostgreSQL), wiremock (HTTP mocks), tokio-tungstenite (WS mock), tonic (gRPC mock), tracing-subscriber (ログキャプチャ), csv (フィクスチャ読み込み)

**ブランチ:** `feat/integration-test-infra`

**参照スペック:** `docs/superpowers/specs/2026-05-02-integration-test-design.md`

---

## 0. Scope と非スコープ

**本 Plan で実装する:**
- `crates/integration-tests` クレート（Cargo.toml, lib.rs, mod 構造）
- DB セットアップ / ティアダウンヘルパー（`#[sqlx::test]` ベース）
- フィクスチャローダー（CSV → price_candles テーブル）
- テスト失敗出力フォーマッター（tracing キャプチャ、DB スナップショット、git diff）
- 全 7 モック: MockExchangeApi, MockGmoFxServer, MockBitflyerWs, MockOandaServer, MockSlackWebhook, MockVegapunk, MockGemini
- Smoke test 1 本（DB + fixture + mock → assert）

**本 Plan で実装しない:**
- Phase 1-4 の個別テストケース（Plan 2-4 で実施）
- CSV フィクスチャデータファイルの作成（Plan 2-4 で各テストと同時に作成）
- TOML 設定フィクスチャファイル（Plan 2 で作成）

---

## File Structure

**新規作成:**
- `crates/integration-tests/Cargo.toml`
- `crates/integration-tests/src/lib.rs`
- `crates/integration-tests/src/helpers/mod.rs`
- `crates/integration-tests/src/helpers/db.rs`
- `crates/integration-tests/src/helpers/fixture_loader.rs`
- `crates/integration-tests/src/helpers/failure_output.rs`
- `crates/integration-tests/src/mocks/mod.rs`
- `crates/integration-tests/src/mocks/exchange_api.rs`
- `crates/integration-tests/src/mocks/gmo_fx_server.rs`
- `crates/integration-tests/src/mocks/bitflyer_ws.rs`
- `crates/integration-tests/src/mocks/oanda_server.rs`
- `crates/integration-tests/src/mocks/slack_webhook.rs`
- `crates/integration-tests/src/mocks/vegapunk.rs`
- `crates/integration-tests/src/mocks/gemini.rs`
- `crates/integration-tests/fixtures/smoke_test.csv`
- `crates/integration-tests/tests/smoke_test.rs`

**変更:**
- `Cargo.toml` (workspace) — `crates/integration-tests` を members に追加、`csv` を workspace deps に追加

---

## Task 1: Create integration-tests crate skeleton

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/integration-tests/Cargo.toml`
- Create: `crates/integration-tests/src/lib.rs`

- [ ] **Step 1: Add crate to workspace members and add csv dep**

In `Cargo.toml` (workspace root), add `"crates/integration-tests"` to members and `csv = "1"` to `[workspace.dependencies]`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/core",
    "crates/db",
    "crates/macro-analyst",
    "crates/market",
    "crates/strategy",
    "crates/executor",
    "crates/app",
    "crates/vegapunk-client",
    "crates/backtest",
    "crates/notify",
    "crates/integration-tests",
]
```

Add to `[workspace.dependencies]`:
```toml
csv = "1"
```

- [ ] **Step 2: Create Cargo.toml for integration-tests**

Create `crates/integration-tests/Cargo.toml`:

```toml
[package]
name = "auto-trader-integration-tests"
version = "0.1.0"
edition.workspace = true
publish = false

# テストオンリークレート — lib は公開 API なし
[lib]
doctest = false

[dependencies]
# internal crates
auto-trader-core = { workspace = true }
auto-trader-db = { workspace = true }
auto-trader-market = { workspace = true }
auto-trader-strategy = { workspace = true }
auto-trader-executor = { workspace = true }
auto-trader-vegapunk = { workspace = true }
auto-trader-notify = { workspace = true }

# async runtime
tokio = { workspace = true }
async-trait = { workspace = true }

# serialization
serde = { workspace = true }
serde_json = { workspace = true }

# database
sqlx = { workspace = true }

# decimal / time / uuid
rust_decimal = { workspace = true }
rust_decimal_macros = "1"
chrono = { workspace = true }
uuid = { workspace = true }

# HTTP mocks
wiremock = { workspace = true }
reqwest = { workspace = true }

# WebSocket mock
tokio-tungstenite = { workspace = true }
futures-util = { workspace = true }

# gRPC mock (Vegapunk)
tonic = { workspace = true }
prost = { workspace = true }

# tracing capture
tracing = { workspace = true }
tracing-subscriber = { workspace = true }

# fixture loading
csv = { workspace = true }

# error handling
anyhow = { workspace = true }

[features]
external-api = []
```

- [ ] **Step 3: Create lib.rs with module structure**

Create `crates/integration-tests/src/lib.rs`:

```rust
//! auto-trader 結合テスト基盤。
//!
//! Phase 1-3 のテストはモックのみで完結し、Phase 4 は `external-api`
//! feature flag を有効にした場合のみ実 API に接続する。

pub mod helpers;
pub mod mocks;
```

- [ ] **Step 4: Create helpers/mod.rs**

Create `crates/integration-tests/src/helpers/mod.rs`:

```rust
pub mod db;
pub mod failure_output;
pub mod fixture_loader;
```

- [ ] **Step 5: Create mocks/mod.rs**

Create `crates/integration-tests/src/mocks/mod.rs`:

```rust
pub mod bitflyer_ws;
pub mod exchange_api;
pub mod gemini;
pub mod gmo_fx_server;
pub mod oanda_server;
pub mod slack_webhook;
pub mod vegapunk;
```

- [ ] **Step 6: Verify compilation**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
mkdir -p crates/integration-tests/src/helpers crates/integration-tests/src/mocks crates/integration-tests/fixtures
# Create placeholder files for modules that don't exist yet
for f in helpers/db helpers/fixture_loader helpers/failure_output mocks/exchange_api mocks/gmo_fx_server mocks/bitflyer_ws mocks/oanda_server mocks/slack_webhook mocks/vegapunk mocks/gemini; do
  echo "// placeholder" > "crates/integration-tests/src/${f}.rs"
done
cargo check -p auto-trader-integration-tests
```

**Run:** `cargo check -p auto-trader-integration-tests`

**Commit:** `feat(test): create integration-tests crate skeleton`

---

## Task 2: DB setup helper

**Files:**
- Create: `crates/integration-tests/src/helpers/db.rs`

- [ ] **Step 1: Write failing test**

Create `crates/integration-tests/tests/smoke_test.rs` (this will fail because `db::snapshot_tables` doesn't exist yet):

```rust
//! Smoke test: DB + fixture + mock の統合動作確認。

use auto_trader_integration_tests::helpers::db;

#[sqlx::test(migrations = "../../migrations")]
async fn db_helper_snapshot_returns_table_contents(pool: sqlx::PgPool) {
    // seed 1 row into trading_accounts
    sqlx::query(
        r#"INSERT INTO trading_accounts
               (id, name, account_type, exchange, strategy,
                initial_balance, current_balance, leverage, currency)
           VALUES (gen_random_uuid(), 'smoke', 'paper', 'gmo_fx', 'bb_mean_revert_v1',
                   100000, 100000, 2, 'JPY')"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let snapshot = db::snapshot_tables(&pool, &["trading_accounts"]).await;
    assert!(
        snapshot.contains("smoke"),
        "snapshot must contain seeded account name: {snapshot}"
    );
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- db_helper_snapshot 2>&1 | tail -20
```

- [ ] **Step 2: Implement db.rs**

Create `crates/integration-tests/src/helpers/db.rs`:

```rust
//! DB ヘルパー: スナップショット取得・シードデータ投入。
//!
//! `#[sqlx::test(migrations = "../../migrations")]` でテストごとに
//! クリーンな DB が渡されるため、明示的な teardown は不要。

use sqlx::PgPool;
use uuid::Uuid;

/// 指定テーブルの全行を JSON 文字列として返す。
/// テスト失敗時の DB 状態ダンプに使用。
pub async fn snapshot_tables(pool: &PgPool, tables: &[&str]) -> String {
    let mut out = String::new();
    for table in tables {
        out.push_str(&format!("--- {table} ---\n"));
        // sqlx は動的テーブル名のパラメータバインドをサポートしないため
        // format! を使用。テーブル名はハードコードされたリテラルのみ受け付ける
        // （テスト基盤内部でのみ使用し、外部入力は通さない）。
        let query = format!(
            "SELECT row_to_json(t) FROM (SELECT * FROM {table}) t"
        );
        let rows: Vec<(serde_json::Value,)> =
            sqlx::query_as(&query).fetch_all(pool).await.unwrap_or_default();
        if rows.is_empty() {
            out.push_str("  (empty)\n");
        } else {
            for (json,) in &rows {
                out.push_str(&format!("  {json}\n"));
            }
        }
    }
    out
}

/// テスト用の trading_account を 1 件シードする。
/// account_type: "paper" or "live"
/// exchange: "bitflyer_cfd", "gmo_fx", "oanda"
pub async fn seed_trading_account(
    pool: &PgPool,
    name: &str,
    account_type: &str,
    exchange: &str,
    strategy: &str,
    initial_balance: i64,
) -> Uuid {
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
    .expect("seed_trading_account failed");
    id
}

/// 標準的なテスト構成: BitflyerCfd paper + GmoFx paper をシードする。
pub async fn seed_standard_accounts(pool: &PgPool) -> StandardAccounts {
    let bitflyer_id = seed_trading_account(
        pool,
        "BTC テスト",
        "paper",
        "bitflyer_cfd",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;
    let gmo_fx_id = seed_trading_account(
        pool,
        "FX テスト",
        "paper",
        "gmo_fx",
        "bb_mean_revert_v1",
        100_000,
    )
    .await;
    StandardAccounts {
        bitflyer_id,
        gmo_fx_id,
    }
}

pub struct StandardAccounts {
    pub bitflyer_id: Uuid,
    pub gmo_fx_id: Uuid,
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- db_helper_snapshot 2>&1 | tail -20
```

**Commit:** `feat(test): add DB helper with snapshot and seed functions`

---

## Task 3: Fixture loader (CSV → price_candles)

**Files:**
- Create: `crates/integration-tests/src/helpers/fixture_loader.rs`
- Create: `crates/integration-tests/fixtures/smoke_test.csv`

- [ ] **Step 1: Write failing test**

Append to `crates/integration-tests/tests/smoke_test.rs`:

```rust
use auto_trader_integration_tests::helpers::fixture_loader;

#[sqlx::test(migrations = "../../migrations")]
async fn fixture_loader_inserts_candles(pool: sqlx::PgPool) {
    let count = fixture_loader::load_price_candles(
        &pool,
        "smoke_test.csv",
        "USD_JPY",
        "M5",
        "oanda",
    )
    .await
    .unwrap();
    assert!(count > 0, "must insert at least 1 candle");

    let db_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM price_candles WHERE pair = 'USD_JPY'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(db_count.0, count as i64);
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- fixture_loader_inserts 2>&1 | tail -20
```

- [ ] **Step 2: Create smoke_test.csv fixture**

Create `crates/integration-tests/fixtures/smoke_test.csv`:

```csv
timestamp,open,high,low,close,volume,bid,ask
2026-01-01T00:00:00Z,150.100,150.200,150.050,150.150,1000,150.100,150.200
2026-01-01T00:05:00Z,150.150,150.300,150.100,150.250,1200,150.200,150.300
2026-01-01T00:10:00Z,150.250,150.350,150.200,150.300,800,150.250,150.350
2026-01-01T00:15:00Z,150.300,150.400,150.250,150.350,900,150.300,150.400
2026-01-01T00:20:00Z,150.350,150.450,150.300,150.400,1100,150.350,150.450
```

- [ ] **Step 3: Implement fixture_loader.rs**

Create `crates/integration-tests/src/helpers/fixture_loader.rs`:

```rust
//! CSV フィクスチャ → price_candles テーブルへのバルクインサート。
//!
//! CSV 形式: timestamp,open,high,low,close,volume,bid,ask
//! bid/ask は price_candles テーブルには含まれないが、フィクスチャデータとして
//! 保持しておき、PriceStore 初期化等で使う場合に備える。

use anyhow::Context;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::PgPool;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone, Deserialize)]
pub struct CandleRow {
    pub timestamp: String,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub volume: String,
    pub bid: String,
    pub ask: String,
}

impl CandleRow {
    pub fn parse_timestamp(&self) -> anyhow::Result<DateTime<Utc>> {
        self.timestamp
            .parse::<DateTime<Utc>>()
            .context("failed to parse timestamp")
    }

    pub fn parse_open(&self) -> anyhow::Result<Decimal> {
        Decimal::from_str(&self.open).context("failed to parse open")
    }

    pub fn parse_high(&self) -> anyhow::Result<Decimal> {
        Decimal::from_str(&self.high).context("failed to parse high")
    }

    pub fn parse_low(&self) -> anyhow::Result<Decimal> {
        Decimal::from_str(&self.low).context("failed to parse low")
    }

    pub fn parse_close(&self) -> anyhow::Result<Decimal> {
        Decimal::from_str(&self.close).context("failed to parse close")
    }

    pub fn parse_volume(&self) -> anyhow::Result<i64> {
        self.volume.parse::<i64>().context("failed to parse volume")
    }

    pub fn parse_bid(&self) -> anyhow::Result<Decimal> {
        Decimal::from_str(&self.bid).context("failed to parse bid")
    }

    pub fn parse_ask(&self) -> anyhow::Result<Decimal> {
        Decimal::from_str(&self.ask).context("failed to parse ask")
    }
}

/// フィクスチャファイルのベースディレクトリを返す。
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// CSV からフィクスチャの生レコードを読み込む。
pub fn read_candle_rows(filename: &str) -> anyhow::Result<Vec<CandleRow>> {
    let path = fixtures_dir().join(filename);
    let mut reader = csv::Reader::from_path(&path)
        .with_context(|| format!("failed to open fixture: {}", path.display()))?;
    let mut rows = Vec::new();
    for result in reader.deserialize() {
        let row: CandleRow =
            result.with_context(|| format!("failed to parse row in {}", path.display()))?;
        rows.push(row);
    }
    Ok(rows)
}

/// CSV フィクスチャを読み込み、price_candles テーブルに INSERT する。
///
/// 戻り値は挿入件数。ON CONFLICT は上書き（テストの冪等性確保）。
pub async fn load_price_candles(
    pool: &PgPool,
    filename: &str,
    pair: &str,
    timeframe: &str,
    exchange: &str,
) -> anyhow::Result<usize> {
    let rows = read_candle_rows(filename)?;
    let count = rows.len();

    for row in &rows {
        let ts = row.parse_timestamp()?;
        let open = row.parse_open()?;
        let high = row.parse_high()?;
        let low = row.parse_low()?;
        let close = row.parse_close()?;
        let volume = row.parse_volume()?;

        sqlx::query(
            r#"INSERT INTO price_candles (pair, timeframe, exchange, open, high, low, close, volume, timestamp)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT (exchange, pair, timeframe, timestamp)
               DO UPDATE SET open = EXCLUDED.open, high = EXCLUDED.high,
                             low = EXCLUDED.low, close = EXCLUDED.close,
                             volume = EXCLUDED.volume"#,
        )
        .bind(pair)
        .bind(timeframe)
        .bind(exchange)
        .bind(open)
        .bind(high)
        .bind(low)
        .bind(close)
        .bind(volume)
        .bind(ts)
        .execute(pool)
        .await
        .with_context(|| format!("failed to insert candle at {ts}"))?;
    }

    Ok(count)
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- fixture_loader_inserts 2>&1 | tail -20
```

**Commit:** `feat(test): add CSV fixture loader for price_candles`

---

## Task 4: Failure output formatter

**Files:**
- Create: `crates/integration-tests/src/helpers/failure_output.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/integration-tests/tests/smoke_test.rs`:

```rust
use auto_trader_integration_tests::helpers::failure_output::{FailureContext, format_failure};

#[sqlx::test(migrations = "../../migrations")]
async fn failure_output_contains_all_sections(pool: sqlx::PgPool) {
    let ctx = FailureContext {
        test_name: "smoke::failure_output_test",
        source_file: file!(),
        source_line: line!(),
        fixture: Some("smoke_test.csv"),
        expected: "1 open trade",
        actual: "0 trades",
    };

    let logs = vec![
        "INFO  strategy warmup: loaded 5 candles".to_string(),
        "WARN  freshness gate rejected".to_string(),
    ];

    let db_snapshot = db::snapshot_tables(&pool, &["trading_accounts", "trades"]).await;

    let output = format_failure(&ctx, &logs, &db_snapshot);

    assert!(output.contains("[FAIL]"), "must contain [FAIL] header");
    assert!(output.contains("smoke::failure_output_test"), "must contain test name");
    assert!(output.contains("smoke_test.csv"), "must contain fixture name");
    assert!(output.contains("expected:"), "must contain expected");
    assert!(output.contains("actual:"), "must contain actual");
    assert!(output.contains("=== application log ==="), "must contain log section");
    assert!(output.contains("freshness gate rejected"), "must contain log content");
    assert!(output.contains("=== db state ==="), "must contain db state section");
    assert!(output.contains("=== git diff"), "must contain git diff section");
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- failure_output_contains 2>&1 | tail -20
```

- [ ] **Step 2: Implement failure_output.rs**

Create `crates/integration-tests/src/helpers/failure_output.rs`:

```rust
//! テスト失敗時の構造化出力。
//!
//! vibepod 自動修正パイプラインが失敗原因を解析するのに十分な
//! 情報を提供する。

use std::process::Command;

/// テスト失敗コンテキスト。各テストが失敗時に構築する。
pub struct FailureContext<'a> {
    pub test_name: &'a str,
    pub source_file: &'a str,
    pub source_line: u32,
    pub fixture: Option<&'a str>,
    pub expected: &'a str,
    pub actual: &'a str,
}

/// 失敗情報を整形文字列にまとめる。
///
/// 含まれる情報:
/// - テスト名 + ソースファイル:行番号
/// - 使用フィクスチャ
/// - 期待値 vs 実際値
/// - テスト中の tracing ログ
/// - 失敗時の DB スナップショット
/// - 直近の git diff
pub fn format_failure(ctx: &FailureContext<'_>, logs: &[String], db_snapshot: &str) -> String {
    let mut out = String::with_capacity(4096);

    // Header
    out.push_str(&format!("[FAIL] {}\n", ctx.test_name));
    out.push_str(&format!("  test: {}:{}\n", ctx.source_file, ctx.source_line));

    if let Some(fixture) = ctx.fixture {
        out.push_str(&format!("  fixture: {fixture}\n"));
    }

    out.push_str(&format!("  expected: {}\n", ctx.expected));
    out.push_str(&format!("  actual: {}\n", ctx.actual));
    out.push('\n');

    // Application log
    out.push_str("  === application log ===\n");
    if logs.is_empty() {
        out.push_str("  (no logs captured)\n");
    } else {
        for line in logs {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out.push('\n');

    // DB state
    out.push_str("  === db state ===\n");
    for line in db_snapshot.lines() {
        out.push_str(&format!("  {line}\n"));
    }
    out.push('\n');

    // Git diff (last 1 commit)
    out.push_str("  === git diff (last 1 commit) ===\n");
    let git_diff = get_git_diff();
    for line in git_diff.lines() {
        out.push_str(&format!("  {line}\n"));
    }

    out
}

/// 直近 1 コミットの diff を取得する。
/// git コマンド失敗時は空文字列を返す（テスト実行を止めない）。
fn get_git_diff() -> String {
    let output = Command::new("git")
        .args(["diff", "HEAD~1", "--stat"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            format!("(git diff failed: {stderr})")
        }
        Err(e) => format!("(git command failed: {e})"),
    }
}

/// tracing ログをキャプチャするための in-memory layer。
///
/// テスト開始時に `init_test_tracing()` で初期化し、
/// テスト終了時に `drain_captured_logs()` でログを回収する。
pub struct TracingCapture {
    logs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl TracingCapture {
    /// tracing subscriber をセットアップし、キャプチャハンドルを返す。
    ///
    /// 注意: `tracing::subscriber::set_default` を使うため、
    /// 同一スレッド内でのみ有効。`#[sqlx::test]` は各テストに
    /// 独立したランタイムを提供するため問題なし。
    pub fn init() -> (Self, tracing::subscriber::DefaultGuard) {
        let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logs_clone = logs.clone();

        let layer = TracingCaptureLayer {
            logs: logs_clone,
        };

        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::Registry::default().with(layer);
        let guard = tracing::subscriber::set_default(subscriber);

        (Self { logs }, guard)
    }

    /// キャプチャしたログ行を返す。
    pub fn drain(&self) -> Vec<String> {
        let mut logs = self.logs.lock().unwrap();
        std::mem::take(&mut *logs)
    }
}

struct TracingCaptureLayer {
    logs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl<S> tracing_subscriber::Layer<S> for TracingCaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = StringVisitor::default();
        event.record(&mut visitor);
        let level = event.metadata().level();
        let target = event.metadata().target();
        let line = format!("{level:<5} {target}: {}", visitor.0);
        if let Ok(mut logs) = self.logs.lock() {
            logs.push(line);
        }
    }
}

#[derive(Default)]
struct StringVisitor(String);

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        if field.name() == "message" {
            self.0.push_str(&format!("{value:?}"));
        } else {
            self.0.push_str(&format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            self.0.push_str(&format!("{}={value}", field.name()));
        }
    }
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- failure_output_contains 2>&1 | tail -20
```

**Commit:** `feat(test): add failure output formatter with tracing capture`

---

## Task 5: MockExchangeApi (configurable per-method responses)

**Files:**
- Create: `crates/integration-tests/src/mocks/exchange_api.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/integration-tests/tests/smoke_test.rs`:

```rust
use auto_trader_integration_tests::mocks::exchange_api::MockExchangeApi;
use auto_trader_market::exchange_api::ExchangeApi;
use rust_decimal_macros::dec;

#[tokio::test]
async fn mock_exchange_api_configurable_responses() {
    let mock = MockExchangeApi::builder()
        .with_positions("FX_BTC_JPY", vec![
            auto_trader_market::bitflyer_private::ExchangePosition {
                product_code: "FX_BTC_JPY".to_string(),
                side: "BUY".to_string(),
                price: dec!(11_500_000),
                size: dec!(0.001),
                commission: dec!(0),
                swap_point_accumulate: dec!(0),
                require_collateral: dec!(0),
                open_date: "2026-01-01T00:00:00".to_string(),
                leverage: dec!(2),
                pnl: dec!(0),
                sfd: dec!(0),
            },
        ])
        .with_collateral(auto_trader_market::bitflyer_private::Collateral {
            collateral: dec!(100_000),
            open_position_pnl: dec!(0),
            require_collateral: dec!(50_000),
            keep_rate: dec!(2.0),
        })
        .build();

    let positions = mock.get_positions("FX_BTC_JPY").await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].size, dec!(0.001));

    let collateral = mock.get_collateral().await.unwrap();
    assert_eq!(collateral.collateral, dec!(100_000));
}

#[tokio::test]
async fn mock_exchange_api_failure_injection() {
    let mock = MockExchangeApi::builder()
        .with_get_positions_failures(2)
        .build();

    // First 2 calls fail
    assert!(mock.get_positions("FX_BTC_JPY").await.is_err());
    assert!(mock.get_positions("FX_BTC_JPY").await.is_err());
    // Third call succeeds (returns empty)
    let positions = mock.get_positions("FX_BTC_JPY").await.unwrap();
    assert!(positions.is_empty());
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_exchange_api 2>&1 | tail -20
```

- [ ] **Step 2: Implement MockExchangeApi**

Create `crates/integration-tests/src/mocks/exchange_api.rs`:

```rust
//! ExchangeApi trait の完全コンフィギュラブルモック。
//!
//! crates/app/src/startup_reconcile.rs の MinimalMock から進化:
//! - 全メソッドに対してレスポンスを設定可能
//! - メソッドごとの失敗注入（N 回失敗 → 成功）
//! - 呼び出し回数カウント

use async_trait::async_trait;
use auto_trader_market::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse,
};
use auto_trader_market::exchange_api::ExchangeApi;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct MockExchangeApi {
    positions: HashMap<String, Vec<ExchangePosition>>,
    collateral: Option<Collateral>,
    send_order_response: Option<SendChildOrderResponse>,
    child_orders: HashMap<String, Vec<ChildOrder>>,
    executions: HashMap<String, Vec<Execution>>,

    // 失敗注入
    get_positions_failures: AtomicU32,
    send_order_failures: AtomicU32,
    get_child_orders_failures: AtomicU32,
    get_executions_failures: AtomicU32,
    get_collateral_failures: AtomicU32,
    cancel_order_failures: AtomicU32,

    // 呼び出しカウント
    pub call_counts: Arc<CallCounts>,
}

pub struct CallCounts {
    pub get_positions: AtomicU32,
    pub send_child_order: AtomicU32,
    pub get_child_orders: AtomicU32,
    pub get_executions: AtomicU32,
    pub get_collateral: AtomicU32,
    pub cancel_child_order: AtomicU32,
    pub sent_orders: Mutex<Vec<SendChildOrderRequest>>,
}

impl CallCounts {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            get_positions: AtomicU32::new(0),
            send_child_order: AtomicU32::new(0),
            get_child_orders: AtomicU32::new(0),
            get_executions: AtomicU32::new(0),
            get_collateral: AtomicU32::new(0),
            cancel_child_order: AtomicU32::new(0),
            sent_orders: Mutex::new(Vec::new()),
        })
    }
}

pub struct MockExchangeApiBuilder {
    positions: HashMap<String, Vec<ExchangePosition>>,
    collateral: Option<Collateral>,
    send_order_response: Option<SendChildOrderResponse>,
    child_orders: HashMap<String, Vec<ChildOrder>>,
    executions: HashMap<String, Vec<Execution>>,
    get_positions_failures: u32,
    send_order_failures: u32,
    get_child_orders_failures: u32,
    get_executions_failures: u32,
    get_collateral_failures: u32,
    cancel_order_failures: u32,
}

impl MockExchangeApi {
    pub fn builder() -> MockExchangeApiBuilder {
        MockExchangeApiBuilder {
            positions: HashMap::new(),
            collateral: None,
            send_order_response: None,
            child_orders: HashMap::new(),
            executions: HashMap::new(),
            get_positions_failures: 0,
            send_order_failures: 0,
            get_child_orders_failures: 0,
            get_executions_failures: 0,
            get_collateral_failures: 0,
            cancel_order_failures: 0,
        }
    }
}

impl MockExchangeApiBuilder {
    pub fn with_positions(mut self, product_code: &str, positions: Vec<ExchangePosition>) -> Self {
        self.positions.insert(product_code.to_string(), positions);
        self
    }

    pub fn with_collateral(mut self, collateral: Collateral) -> Self {
        self.collateral = Some(collateral);
        self
    }

    pub fn with_send_order_response(mut self, response: SendChildOrderResponse) -> Self {
        self.send_order_response = Some(response);
        self
    }

    pub fn with_child_orders(mut self, acceptance_id: &str, orders: Vec<ChildOrder>) -> Self {
        self.child_orders.insert(acceptance_id.to_string(), orders);
        self
    }

    pub fn with_executions(mut self, acceptance_id: &str, executions: Vec<Execution>) -> Self {
        self.executions
            .insert(acceptance_id.to_string(), executions);
        self
    }

    pub fn with_get_positions_failures(mut self, n: u32) -> Self {
        self.get_positions_failures = n;
        self
    }

    pub fn with_send_order_failures(mut self, n: u32) -> Self {
        self.send_order_failures = n;
        self
    }

    pub fn with_get_child_orders_failures(mut self, n: u32) -> Self {
        self.get_child_orders_failures = n;
        self
    }

    pub fn with_get_executions_failures(mut self, n: u32) -> Self {
        self.get_executions_failures = n;
        self
    }

    pub fn with_get_collateral_failures(mut self, n: u32) -> Self {
        self.get_collateral_failures = n;
        self
    }

    pub fn with_cancel_order_failures(mut self, n: u32) -> Self {
        self.cancel_order_failures = n;
        self
    }

    pub fn build(self) -> MockExchangeApi {
        MockExchangeApi {
            positions: self.positions,
            collateral: self.collateral,
            send_order_response: self.send_order_response,
            child_orders: self.child_orders,
            executions: self.executions,
            get_positions_failures: AtomicU32::new(self.get_positions_failures),
            send_order_failures: AtomicU32::new(self.send_order_failures),
            get_child_orders_failures: AtomicU32::new(self.get_child_orders_failures),
            get_executions_failures: AtomicU32::new(self.get_executions_failures),
            get_collateral_failures: AtomicU32::new(self.get_collateral_failures),
            cancel_order_failures: AtomicU32::new(self.cancel_order_failures),
            call_counts: CallCounts::new(),
        }
    }
}

/// 失敗カウンターをデクリメントし、まだ > 0 だったら Err を返す。
fn check_failure(counter: &AtomicU32, method_name: &str) -> Result<(), anyhow::Error> {
    let did_decrement = counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
            if v > 0 {
                Some(v - 1)
            } else {
                None
            }
        })
        .is_ok();
    if did_decrement {
        anyhow::bail!("mock {method_name} injected failure");
    }
    Ok(())
}

#[async_trait]
impl ExchangeApi for MockExchangeApi {
    async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        self.call_counts
            .send_child_order
            .fetch_add(1, Ordering::SeqCst);
        self.call_counts.sent_orders.lock().await.push(req);
        check_failure(&self.send_order_failures, "send_child_order")?;
        Ok(self
            .send_order_response
            .clone()
            .unwrap_or(SendChildOrderResponse {
                child_order_acceptance_id: format!("mock-order-{}", uuid::Uuid::new_v4()),
            }))
    }

    async fn get_child_orders(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        self.call_counts
            .get_child_orders
            .fetch_add(1, Ordering::SeqCst);
        check_failure(&self.get_child_orders_failures, "get_child_orders")?;
        Ok(self
            .child_orders
            .get(child_order_acceptance_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        self.call_counts
            .get_executions
            .fetch_add(1, Ordering::SeqCst);
        check_failure(&self.get_executions_failures, "get_executions")?;
        Ok(self
            .executions
            .get(child_order_acceptance_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn get_positions(&self, product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        self.call_counts
            .get_positions
            .fetch_add(1, Ordering::SeqCst);
        check_failure(&self.get_positions_failures, "get_positions")?;
        Ok(self
            .positions
            .get(product_code)
            .cloned()
            .unwrap_or_default())
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        self.call_counts
            .get_collateral
            .fetch_add(1, Ordering::SeqCst);
        check_failure(&self.get_collateral_failures, "get_collateral")?;
        self.collateral
            .clone()
            .ok_or_else(|| anyhow::anyhow!("mock get_collateral: no collateral configured"))
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        self.call_counts
            .cancel_child_order
            .fetch_add(1, Ordering::SeqCst);
        check_failure(&self.cancel_order_failures, "cancel_child_order")?;
        Ok(())
    }
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_exchange_api 2>&1 | tail -20
```

**Commit:** `feat(test): add configurable MockExchangeApi with failure injection`

---

## Task 6: MockGmoFxServer (wiremock HTTP)

**Files:**
- Create: `crates/integration-tests/src/mocks/gmo_fx_server.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/integration-tests/tests/smoke_test.rs`:

```rust
use auto_trader_integration_tests::mocks::gmo_fx_server::{MockGmoFxServer, GmoFxScenario};

#[tokio::test]
async fn mock_gmo_fx_server_normal_ticker() {
    let server = MockGmoFxServer::start(GmoFxScenario::NormalTicker {
        symbol: "USD_JPY".to_string(),
        bid: "150.100".to_string(),
        ask: "150.200".to_string(),
    })
    .await;

    let resp = reqwest::get(format!("{}/public/v1/ticker", server.base_url()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 0);
    assert_eq!(body["data"][0]["symbol"], "USD_JPY");
}

#[tokio::test]
async fn mock_gmo_fx_server_maintenance() {
    let server = MockGmoFxServer::start(GmoFxScenario::Maintenance).await;

    let resp = reqwest::get(format!("{}/public/v1/ticker", server.base_url()))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 5);
    assert!(body["data"].as_array().unwrap().is_empty());
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_gmo_fx 2>&1 | tail -20
```

- [ ] **Step 2: Implement MockGmoFxServer**

Create `crates/integration-tests/src/mocks/gmo_fx_server.rs`:

```rust
//! GMO FX Public API のモック (wiremock ベース)。
//!
//! シナリオ:
//! - NormalTicker: 正常 ticker レスポンス
//! - Maintenance: status=5 (メンテナンス中)
//! - MarketClosed: status=0 + "CLOSE" ステータス
//! - InvalidJson: 不正 JSON
//! - HttpError: 指定ステータスコード
//! - Delayed: レスポンス遅延

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub enum GmoFxScenario {
    NormalTicker {
        symbol: String,
        bid: String,
        ask: String,
    },
    Maintenance,
    MarketClosed {
        symbol: String,
    },
    InvalidJson,
    HttpError {
        status: u16,
    },
    Delayed {
        symbol: String,
        bid: String,
        ask: String,
        delay_ms: u64,
    },
    ConnectionRefused,
}

pub struct MockGmoFxServer {
    server: MockServer,
}

impl MockGmoFxServer {
    pub async fn start(scenario: GmoFxScenario) -> Self {
        let server = MockServer::start().await;

        match scenario {
            GmoFxScenario::NormalTicker { symbol, bid, ask } => {
                let body = serde_json::json!({
                    "status": 0,
                    "data": [{
                        "symbol": symbol,
                        "ask": ask,
                        "bid": bid,
                        "timestamp": "2026-01-01T00:00:00.000Z",
                        "status": "OPEN"
                    }],
                    "responsetime": "2026-01-01T00:00:00.000Z"
                });
                Mock::given(method("GET"))
                    .and(path("/public/v1/ticker"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                    .mount(&server)
                    .await;
            }
            GmoFxScenario::Maintenance => {
                let body = serde_json::json!({
                    "status": 5,
                    "data": [],
                    "responsetime": "2026-01-01T00:00:00.000Z"
                });
                Mock::given(method("GET"))
                    .and(path("/public/v1/ticker"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                    .mount(&server)
                    .await;
            }
            GmoFxScenario::MarketClosed { symbol } => {
                let body = serde_json::json!({
                    "status": 0,
                    "data": [{
                        "symbol": symbol,
                        "ask": "0",
                        "bid": "0",
                        "timestamp": "2026-01-01T00:00:00.000Z",
                        "status": "CLOSE"
                    }],
                    "responsetime": "2026-01-01T00:00:00.000Z"
                });
                Mock::given(method("GET"))
                    .and(path("/public/v1/ticker"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                    .mount(&server)
                    .await;
            }
            GmoFxScenario::InvalidJson => {
                Mock::given(method("GET"))
                    .and(path("/public/v1/ticker"))
                    .respond_with(
                        ResponseTemplate::new(200).set_body_string("not valid json {{{"),
                    )
                    .mount(&server)
                    .await;
            }
            GmoFxScenario::HttpError { status } => {
                Mock::given(method("GET"))
                    .and(path("/public/v1/ticker"))
                    .respond_with(ResponseTemplate::new(status))
                    .mount(&server)
                    .await;
            }
            GmoFxScenario::Delayed {
                symbol,
                bid,
                ask,
                delay_ms,
            } => {
                let body = serde_json::json!({
                    "status": 0,
                    "data": [{
                        "symbol": symbol,
                        "ask": ask,
                        "bid": bid,
                        "timestamp": "2026-01-01T00:00:00.000Z",
                        "status": "OPEN"
                    }],
                    "responsetime": "2026-01-01T00:00:00.000Z"
                });
                Mock::given(method("GET"))
                    .and(path("/public/v1/ticker"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(&body)
                            .set_delay(std::time::Duration::from_millis(delay_ms)),
                    )
                    .mount(&server)
                    .await;
            }
            GmoFxScenario::ConnectionRefused => {
                // server を即 drop しないため、mount なし → 404 を返す
                // 真の「接続拒否」は MockServer では表現できないため、
                // テスト側で base_url を不正ポートに差し替える
            }
        }

        Self { server }
    }

    pub fn base_url(&self) -> String {
        self.server.uri()
    }
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_gmo_fx 2>&1 | tail -20
```

**Commit:** `feat(test): add MockGmoFxServer with wiremock scenarios`

---

## Task 7: MockBitflyerWs (WebSocket server)

**Files:**
- Create: `crates/integration-tests/src/mocks/bitflyer_ws.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/integration-tests/tests/smoke_test.rs`:

```rust
use auto_trader_integration_tests::mocks::bitflyer_ws::{MockBitflyerWs, BitflyerWsScenario};
use futures_util::StreamExt;
use tokio_tungstenite::connect_async;

#[tokio::test]
async fn mock_bitflyer_ws_sends_ticks() {
    let server = MockBitflyerWs::start(BitflyerWsScenario::NormalTicks {
        product_code: "FX_BTC_JPY".to_string(),
        ticks: vec![
            ("11500000", "11500100", "11499900"),
            ("11500200", "11500300", "11500100"),
        ],
    })
    .await;

    let (mut ws, _) = connect_async(&server.ws_url()).await.unwrap();

    // subscribe message (bitFlyer JSON-RPC format)
    let subscribe = serde_json::json!({
        "method": "subscribe",
        "params": { "channel": "lightning_ticker_FX_BTC_JPY" }
    });
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    ws.send(Message::Text(subscribe.to_string())).await.unwrap();

    // receive 2 ticks
    let msg1 = ws.next().await.unwrap().unwrap();
    assert!(msg1.is_text(), "first message must be text");
    let parsed: serde_json::Value = serde_json::from_str(&msg1.to_text().unwrap()).unwrap();
    assert_eq!(parsed["params"]["message"]["product_code"], "FX_BTC_JPY");

    let msg2 = ws.next().await.unwrap().unwrap();
    assert!(msg2.is_text(), "second message must be text");
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_bitflyer_ws 2>&1 | tail -20
```

- [ ] **Step 2: Implement MockBitflyerWs**

Create `crates/integration-tests/src/mocks/bitflyer_ws.rs`:

```rust
//! bitFlyer Lightning WebSocket のモック。
//!
//! シナリオ:
//! - NormalTicks: subscribe 後に指定 tick を送信
//! - DisconnectAfter: N メッセージ後に切断
//! - HeartbeatTimeout: heartbeat を送らない
//! - InvalidMessage: 不正 JSON 送信

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

pub enum BitflyerWsScenario {
    NormalTicks {
        product_code: String,
        ticks: Vec<(&'static str, &'static str, &'static str)>, // (ltp, best_ask, best_bid)
    },
    DisconnectAfter {
        product_code: String,
        ticks_before_disconnect: usize,
    },
    HeartbeatTimeout,
    InvalidMessage,
}

pub struct MockBitflyerWs {
    addr: SocketAddr,
    _handle: tokio::task::JoinHandle<()>,
}

impl MockBitflyerWs {
    pub async fn start(scenario: BitflyerWsScenario) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            // 1 接続のみ受け付ける（テスト用途）
            if let Ok((stream, _)) = listener.accept().await {
                let ws_stream = match accept_async(stream).await {
                    Ok(ws) => ws,
                    Err(_) => return,
                };
                let (mut write, mut read) = ws_stream.split();

                match scenario {
                    BitflyerWsScenario::NormalTicks {
                        product_code,
                        ticks,
                    } => {
                        // subscribe メッセージを待つ
                        let _sub = read.next().await;

                        for (ltp, best_ask, best_bid) in &ticks {
                            let msg = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "channelMessage",
                                "params": {
                                    "channel": format!("lightning_ticker_{product_code}"),
                                    "message": {
                                        "product_code": product_code,
                                        "best_bid": best_bid.to_string(),
                                        "best_ask": best_ask.to_string(),
                                        "ltp": ltp.to_string(),
                                        "volume": "1000",
                                        "timestamp": "2026-01-01T00:00:00.0000000Z"
                                    }
                                }
                            });
                            let send_result =
                                write.send(Message::Text(msg.to_string())).await;
                            if send_result.is_err() {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    }
                    BitflyerWsScenario::DisconnectAfter {
                        product_code,
                        ticks_before_disconnect,
                    } => {
                        let _sub = read.next().await;

                        for i in 0..ticks_before_disconnect {
                            let msg = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "channelMessage",
                                "params": {
                                    "channel": format!("lightning_ticker_{product_code}"),
                                    "message": {
                                        "product_code": product_code,
                                        "best_bid": "11500000",
                                        "best_ask": "11500100",
                                        "ltp": format!("{}", 11500000 + i * 100),
                                        "volume": "1000",
                                        "timestamp": "2026-01-01T00:00:00.0000000Z"
                                    }
                                }
                            });
                            let send_result =
                                write.send(Message::Text(msg.to_string())).await;
                            if send_result.is_err() {
                                break;
                            }
                        }
                        // 明示的に close
                        let _ = write.close().await;
                    }
                    BitflyerWsScenario::HeartbeatTimeout => {
                        // subscribe を受け取った後、何も送らない（タイムアウトを誘発）
                        let _sub = read.next().await;
                        // 無限待機 — テスト側がタイムアウトで終了する想定
                        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                    }
                    BitflyerWsScenario::InvalidMessage => {
                        let _sub = read.next().await;
                        let _ = write
                            .send(Message::Text("not json {{invalid".to_string()))
                            .await;
                    }
                }
            }
        });

        Self {
            addr,
            _handle: handle,
        }
    }

    pub fn ws_url(&self) -> String {
        format!("ws://{}", self.addr)
    }
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_bitflyer_ws 2>&1 | tail -20
```

**Commit:** `feat(test): add MockBitflyerWs with WebSocket scenarios`

---

## Task 8: MockOandaServer (wiremock HTTP)

**Files:**
- Create: `crates/integration-tests/src/mocks/oanda_server.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/integration-tests/tests/smoke_test.rs`:

```rust
use auto_trader_integration_tests::mocks::oanda_server::{MockOandaServer, OandaScenario};

#[tokio::test]
async fn mock_oanda_server_normal_candles() {
    let server = MockOandaServer::start(OandaScenario::NormalCandles {
        instrument: "USD_JPY".to_string(),
        candles: vec![
            ("150.100", "150.200", "150.050", "150.150", true),
            ("150.150", "150.300", "150.100", "150.250", true),
        ],
    })
    .await;

    let resp = reqwest::Client::new()
        .get(format!(
            "{}/v3/accounts/test/instruments/USD_JPY/candles",
            server.base_url()
        ))
        .header("Authorization", "Bearer test-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["candles"].as_array().unwrap().len(), 2);
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_oanda 2>&1 | tail -20
```

- [ ] **Step 2: Implement MockOandaServer**

Create `crates/integration-tests/src/mocks/oanda_server.rs`:

```rust
//! OANDA REST API のモック (wiremock ベース)。
//!
//! シナリオ:
//! - NormalCandles: 正常 candle レスポンス
//! - ParseError: 不正 JSON
//! - Timeout: レスポンス遅延
//! - HttpError: 指定ステータスコード

use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub enum OandaScenario {
    NormalCandles {
        instrument: String,
        candles: Vec<(&'static str, &'static str, &'static str, &'static str, bool)>, // (o, h, l, c, complete)
    },
    ParseError,
    Timeout {
        delay_ms: u64,
    },
    HttpError {
        status: u16,
    },
}

pub struct MockOandaServer {
    server: MockServer,
}

impl MockOandaServer {
    pub async fn start(scenario: OandaScenario) -> Self {
        let server = MockServer::start().await;

        match scenario {
            OandaScenario::NormalCandles {
                instrument: _,
                candles,
            } => {
                let candle_json: Vec<serde_json::Value> = candles
                    .iter()
                    .map(|(o, h, l, c, complete)| {
                        serde_json::json!({
                            "time": "2026-01-01T00:00:00.000000000Z",
                            "volume": 100,
                            "mid": { "o": o, "h": h, "l": l, "c": c },
                            "complete": complete
                        })
                    })
                    .collect();

                let body = serde_json::json!({ "candles": candle_json });
                Mock::given(method("GET"))
                    .and(path_regex(r"/v3/accounts/.+/instruments/.+/candles"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                    .mount(&server)
                    .await;
            }
            OandaScenario::ParseError => {
                Mock::given(method("GET"))
                    .and(path_regex(r"/v3/accounts/.+/instruments/.+/candles"))
                    .respond_with(
                        ResponseTemplate::new(200).set_body_string("{{invalid json"),
                    )
                    .mount(&server)
                    .await;
            }
            OandaScenario::Timeout { delay_ms } => {
                Mock::given(method("GET"))
                    .and(path_regex(r"/v3/accounts/.+/instruments/.+/candles"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(&serde_json::json!({ "candles": [] }))
                            .set_delay(std::time::Duration::from_millis(delay_ms)),
                    )
                    .mount(&server)
                    .await;
            }
            OandaScenario::HttpError { status } => {
                Mock::given(method("GET"))
                    .and(path_regex(r"/v3/accounts/.+/instruments/.+/candles"))
                    .respond_with(ResponseTemplate::new(status))
                    .mount(&server)
                    .await;
            }
        }

        Self { server }
    }

    pub fn base_url(&self) -> String {
        self.server.uri()
    }
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_oanda 2>&1 | tail -20
```

**Commit:** `feat(test): add MockOandaServer with wiremock scenarios`

---

## Task 9: MockSlackWebhook (wiremock HTTP)

**Files:**
- Create: `crates/integration-tests/src/mocks/slack_webhook.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/integration-tests/tests/smoke_test.rs`:

```rust
use auto_trader_integration_tests::mocks::slack_webhook::MockSlackWebhook;
use auto_trader_notify::{Notifier, NotifyEvent, OrderFilledEvent};
use auto_trader_core::types::{Direction, Exchange, Pair};
use chrono::Utc;

#[tokio::test]
async fn mock_slack_webhook_captures_body() {
    let mock = MockSlackWebhook::start().await;

    let notifier = Notifier::new(Some(mock.webhook_url()));
    let event = NotifyEvent::OrderFilled(OrderFilledEvent {
        account_name: "テスト口座".to_string(),
        exchange: Exchange::BitflyerCfd,
        trade_id: uuid::Uuid::nil(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        quantity: dec!(0.01),
        price: dec!(11_500_000),
        at: Utc::now(),
    });
    notifier.send(event).await.unwrap();

    let bodies = mock.received_bodies().await;
    assert_eq!(bodies.len(), 1, "must capture exactly 1 webhook call");
    assert!(
        bodies[0].contains("約定"),
        "body must contain order filled text"
    );
}

#[tokio::test]
async fn mock_slack_webhook_error_scenario() {
    let mock = MockSlackWebhook::start_with_status(500).await;

    let notifier = Notifier::new(Some(mock.webhook_url()));
    let event = NotifyEvent::OrderFailed(auto_trader_notify::OrderFailedEvent {
        account_name: "テスト口座".to_string(),
        exchange: Exchange::BitflyerCfd,
        strategy_name: "test".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        reason: "test error".to_string(),
    });
    let result = notifier.send(event).await;
    assert!(result.is_err(), "must return error on 500");
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_slack 2>&1 | tail -20
```

- [ ] **Step 2: Implement MockSlackWebhook**

Create `crates/integration-tests/src/mocks/slack_webhook.rs`:

```rust
//! Slack Webhook のモック (wiremock ベース)。
//!
//! ボディキャプチャ機能付き: 送信された Slack メッセージを後から
//! assert できる。

use std::sync::Arc;
use tokio::sync::Mutex;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

pub struct MockSlackWebhook {
    server: MockServer,
    captured_bodies: Arc<Mutex<Vec<String>>>,
}

impl MockSlackWebhook {
    /// 正常 (200 OK) を返すモック。送信ボディをキャプチャする。
    pub async fn start() -> Self {
        Self::start_with_status(200).await
    }

    /// 指定ステータスコードを返すモック。
    pub async fn start_with_status(status: u16) -> Self {
        let server = MockServer::start().await;
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;

        // wiremock の received_requests で後からボディを取得
        Self {
            server,
            captured_bodies: captured_clone,
        }
    }

    pub fn webhook_url(&self) -> String {
        self.server.uri()
    }

    /// サーバーが受信した POST ボディ一覧を返す。
    pub async fn received_bodies(&self) -> Vec<String> {
        let requests = self.server.received_requests().await.unwrap_or_default();
        requests
            .iter()
            .filter(|r| r.method == wiremock::http::Method::POST)
            .map(|r| String::from_utf8_lossy(&r.body).to_string())
            .collect()
    }
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_slack 2>&1 | tail -20
```

**Commit:** `feat(test): add MockSlackWebhook with body capture`

---

## Task 10: MockVegapunk (tonic gRPC server)

**Files:**
- Create: `crates/integration-tests/src/mocks/vegapunk.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/integration-tests/tests/smoke_test.rs`:

```rust
use auto_trader_integration_tests::mocks::vegapunk::MockVegapunk;

#[tokio::test]
async fn mock_vegapunk_ingest_and_search() {
    let mock = MockVegapunk::start().await;

    let mut client = auto_trader_vegapunk::client::VegapunkClient::connect(
        &mock.endpoint(),
        "test-schema",
        None,
    )
    .await
    .unwrap();

    let ingest_resp = client
        .ingest_raw("test content", "integration_test", "test-channel", "2026-01-01T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(ingest_resp.chunk_count, 1);

    let search_resp = client.search("test query", "local", 10).await.unwrap();
    assert!(!search_resp.search_id.is_empty());

    client.feedback(&search_resp.search_id, 5, "good").await.unwrap();
    client.merge().await.unwrap();
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_vegapunk 2>&1 | tail -20
```

- [ ] **Step 2: Implement MockVegapunk**

Create `crates/integration-tests/src/mocks/vegapunk.rs`:

```rust
//! Vegapunk GraphRAG Engine の gRPC モック (tonic ベース)。
//!
//! VegapunkClient が呼ぶ 4 メソッド (ingest_raw, search, feedback, merge)
//! に対して固定レスポンスを返す。

use auto_trader_vegapunk::proto::graph_rag_engine_server::{GraphRagEngine, GraphRagEngineServer};
use auto_trader_vegapunk::proto::*;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

pub struct MockVegapunk {
    addr: SocketAddr,
    _handle: tokio::task::JoinHandle<()>,
    pub state: Arc<MockVegapunkState>,
}

pub struct MockVegapunkState {
    pub ingest_count: AtomicU32,
    pub search_count: AtomicU32,
    pub feedback_count: AtomicU32,
    pub merge_count: AtomicU32,
    pub ingested_texts: Mutex<Vec<String>>,
    pub should_fail: std::sync::atomic::AtomicBool,
}

impl MockVegapunkState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            ingest_count: AtomicU32::new(0),
            search_count: AtomicU32::new(0),
            feedback_count: AtomicU32::new(0),
            merge_count: AtomicU32::new(0),
            ingested_texts: Mutex::new(Vec::new()),
            should_fail: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

struct VegapunkService {
    state: Arc<MockVegapunkState>,
}

#[tonic::async_trait]
impl GraphRagEngine for VegapunkService {
    async fn ingest(
        &self,
        _req: Request<IngestRequest>,
    ) -> Result<Response<IngestResponse>, Status> {
        if self.state.should_fail.load(Ordering::SeqCst) {
            return Err(Status::internal("mock failure"));
        }
        self.state.ingest_count.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(IngestResponse {
            ingested_count: 1,
            job_id: None,
        }))
    }

    async fn search(
        &self,
        _req: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        if self.state.should_fail.load(Ordering::SeqCst) {
            return Err(Status::internal("mock failure"));
        }
        self.state.search_count.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(SearchResponse {
            results: vec![SearchResultItem {
                r#type: "mock".to_string(),
                id: Some("mock-id".to_string()),
                text: Some("mock search result".to_string()),
                score: Some(0.95),
                person: None,
                timestamp: None,
                summary: None,
                channel: None,
                decided_at: None,
                rationales: vec![],
            }],
            search_id: "mock-search-id".to_string(),
            total_count: 1,
            similar_patterns: vec![],
        }))
    }

    async fn upsert_nodes(
        &self,
        _req: Request<UpsertNodesRequest>,
    ) -> Result<Response<UpsertNodesResponse>, Status> {
        Ok(Response::new(UpsertNodesResponse { upserted_count: 0 }))
    }

    async fn upsert_edges(
        &self,
        _req: Request<UpsertEdgesRequest>,
    ) -> Result<Response<UpsertEdgesResponse>, Status> {
        Ok(Response::new(UpsertEdgesResponse { upserted_count: 0 }))
    }

    async fn upsert_vectors(
        &self,
        _req: Request<UpsertVectorsRequest>,
    ) -> Result<Response<UpsertVectorsResponse>, Status> {
        Ok(Response::new(UpsertVectorsResponse { upserted_count: 0 }))
    }

    async fn merge(
        &self,
        _req: Request<MergeRequest>,
    ) -> Result<Response<MergeResponse>, Status> {
        if self.state.should_fail.load(Ordering::SeqCst) {
            return Err(Status::internal("mock failure"));
        }
        self.state.merge_count.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(MergeResponse {}))
    }

    async fn rebuild(
        &self,
        _req: Request<RebuildRequest>,
    ) -> Result<Response<RebuildResponse>, Status> {
        Ok(Response::new(RebuildResponse {}))
    }

    async fn backup(
        &self,
        _req: Request<BackupRequest>,
    ) -> Result<Response<BackupResponse>, Status> {
        Ok(Response::new(BackupResponse {}))
    }

    async fn migrate(
        &self,
        _req: Request<MigrateRequest>,
    ) -> Result<Response<MigrateResponse>, Status> {
        Ok(Response::new(MigrateResponse {}))
    }

    async fn feedback(
        &self,
        _req: Request<FeedbackRequest>,
    ) -> Result<Response<FeedbackResponse>, Status> {
        if self.state.should_fail.load(Ordering::SeqCst) {
            return Err(Status::internal("mock failure"));
        }
        self.state.feedback_count.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(FeedbackResponse {}))
    }

    async fn get_needs_review(
        &self,
        _req: Request<GetNeedsReviewRequest>,
    ) -> Result<Response<GetNeedsReviewResponse>, Status> {
        Ok(Response::new(GetNeedsReviewResponse { items: vec![] }))
    }

    async fn resolve_match(
        &self,
        _req: Request<ResolveMatchRequest>,
    ) -> Result<Response<ResolveMatchResponse>, Status> {
        Ok(Response::new(ResolveMatchResponse {}))
    }

    async fn get_job_status(
        &self,
        _req: Request<GetJobStatusRequest>,
    ) -> Result<Response<GetJobStatusResponse>, Status> {
        Ok(Response::new(GetJobStatusResponse {
            msg_id: String::new(),
            jobs: vec![],
            overall_status: "completed".to_string(),
        }))
    }

    async fn ingest_raw(
        &self,
        req: Request<IngestRawRequest>,
    ) -> Result<Response<IngestRawResponse>, Status> {
        if self.state.should_fail.load(Ordering::SeqCst) {
            return Err(Status::internal("mock failure"));
        }
        self.state.ingest_count.fetch_add(1, Ordering::SeqCst);
        let text = req.into_inner().text;
        self.state.ingested_texts.lock().await.push(text);
        Ok(Response::new(IngestRawResponse {
            chunk_count: 1,
            msg_ids: vec!["mock-msg-id".to_string()],
        }))
    }

    async fn ingest_file(
        &self,
        _req: Request<IngestFileRequest>,
    ) -> Result<Response<IngestFileResponse>, Status> {
        Ok(Response::new(IngestFileResponse {
            job_id: "mock-job-id".to_string(),
        }))
    }

    async fn delete_schema(
        &self,
        _req: Request<DeleteSchemaRequest>,
    ) -> Result<Response<DeleteSchemaResponse>, Status> {
        Ok(Response::new(DeleteSchemaResponse {
            deleted_nodes: 0,
            deleted_edges: 0,
            deleted_vectors: 0,
            deleted_cross_schema_edges: 0,
            dry_run: true,
            raw_messages_count: 0,
            deleted_raw_messages: 0,
        }))
    }

    async fn reingest(
        &self,
        _req: Request<ReingestRequest>,
    ) -> Result<Response<ReingestResponse>, Status> {
        Ok(Response::new(ReingestResponse { message_count: 0 }))
    }

    async fn improve_prompts(
        &self,
        _req: Request<ImprovePromptsRequest>,
    ) -> Result<Response<ImprovePromptsResponse>, Status> {
        Ok(Response::new(ImprovePromptsResponse {
            applied: false,
            feedback_count: 0,
            reason: "mock".to_string(),
        }))
    }

    async fn create_schema(
        &self,
        _req: Request<CreateSchemaRequest>,
    ) -> Result<Response<CreateSchemaResponse>, Status> {
        Ok(Response::new(CreateSchemaResponse {
            name: "mock".to_string(),
        }))
    }

    async fn get_schema(
        &self,
        _req: Request<GetSchemaRequest>,
    ) -> Result<Response<GetSchemaResponse>, Status> {
        Ok(Response::new(GetSchemaResponse {
            name: "mock".to_string(),
            schema_yaml: String::new(),
            version: 1,
            description: String::new(),
        }))
    }

    async fn list_schemas(
        &self,
        _req: Request<ListSchemasRequest>,
    ) -> Result<Response<ListSchemasResponse>, Status> {
        Ok(Response::new(ListSchemasResponse { schemas: vec![] }))
    }

    async fn update_schema(
        &self,
        _req: Request<UpdateSchemaRequest>,
    ) -> Result<Response<UpdateSchemaResponse>, Status> {
        Ok(Response::new(UpdateSchemaResponse {
            name: "mock".to_string(),
            dry_run: true,
            is_additive: true,
            diff: None,
            job_id: None,
        }))
    }

    async fn list_schema_templates(
        &self,
        _req: Request<ListSchemaTemplatesRequest>,
    ) -> Result<Response<ListSchemaTemplatesResponse>, Status> {
        Ok(Response::new(ListSchemaTemplatesResponse {
            templates: vec![],
        }))
    }

    async fn get_schema_migration_status(
        &self,
        _req: Request<GetSchemaMigrationStatusRequest>,
    ) -> Result<Response<GetSchemaMigrationStatusResponse>, Status> {
        Ok(Response::new(GetSchemaMigrationStatusResponse {
            status: "completed".to_string(),
            error: None,
        }))
    }

    async fn purge_raw_messages(
        &self,
        _req: Request<PurgeRawMessagesRequest>,
    ) -> Result<Response<PurgeRawMessagesResponse>, Status> {
        Ok(Response::new(PurgeRawMessagesResponse {
            schema: "mock".to_string(),
            deleted_count: 0,
            dry_run: true,
        }))
    }

    async fn set_maintenance_mode(
        &self,
        _req: Request<SetMaintenanceModeRequest>,
    ) -> Result<Response<MaintenanceModeResponse>, Status> {
        Ok(Response::new(MaintenanceModeResponse { enabled: false }))
    }

    async fn get_maintenance_mode(
        &self,
        _req: Request<GetMaintenanceModeRequest>,
    ) -> Result<Response<MaintenanceModeResponse>, Status> {
        Ok(Response::new(MaintenanceModeResponse { enabled: false }))
    }
}

impl MockVegapunk {
    pub async fn start() -> Self {
        let state = MockVegapunkState::new();
        let service = VegapunkService {
            state: state.clone(),
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
            tonic::transport::Server::builder()
                .add_service(GraphRagEngineServer::new(service))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        // gRPC サーバーの起動を待つ
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        Self {
            addr,
            _handle: handle,
            state,
        }
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }
}
```

Note: `tokio-stream` の依存を `Cargo.toml` に追加する必要がある。

`crates/integration-tests/Cargo.toml` の `[dependencies]` に追加:
```toml
tokio-stream = "0.1"
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_vegapunk 2>&1 | tail -20
```

**Commit:** `feat(test): add MockVegapunk gRPC server`

---

## Task 11: MockGemini (wiremock HTTP)

**Files:**
- Create: `crates/integration-tests/src/mocks/gemini.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/integration-tests/tests/smoke_test.rs`:

```rust
use auto_trader_integration_tests::mocks::gemini::{MockGemini, GeminiScenario};

#[tokio::test]
async fn mock_gemini_parameter_proposal() {
    let server = MockGemini::start(GeminiScenario::ParameterProposal {
        entry_period: 15,
        exit_period: 8,
        reasoning: "Volatility suggests tighter channels".to_string(),
    })
    .await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/v1beta/models/gemini-2.5-flash:generateContent",
            server.base_url()
        ))
        .header("x-goog-api-key", "test-key")
        .json(&serde_json::json!({"contents": []}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let text = body["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("entry_period"));
}

#[tokio::test]
async fn mock_gemini_swing_signal() {
    let server = MockGemini::start(GeminiScenario::SwingSignal {
        direction: "long".to_string(),
        confidence: 0.85,
    })
    .await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/v1beta/models/gemini-2.5-flash:generateContent",
            server.base_url()
        ))
        .header("x-goog-api-key", "test-key")
        .json(&serde_json::json!({"contents": []}))
        .send()
        .await
        .unwrap();

    let body: serde_json::Value = resp.json().await.unwrap();
    let text = body["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("long"));
}

#[tokio::test]
async fn mock_gemini_invalid_response() {
    let server = MockGemini::start(GeminiScenario::InvalidResponse).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/v1beta/models/gemini-2.5-flash:generateContent",
            server.base_url()
        ))
        .header("x-goog-api-key", "test-key")
        .json(&serde_json::json!({"contents": []}))
        .send()
        .await
        .unwrap();

    let body: serde_json::Value = resp.json().await.unwrap();
    let text = body["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    // 不正 JSON をパースすると失敗するはず
    assert!(serde_json::from_str::<serde_json::Value>(text).is_err());
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_gemini 2>&1 | tail -20
```

- [ ] **Step 2: Implement MockGemini**

Create `crates/integration-tests/src/mocks/gemini.rs`:

```rust
//! Gemini API のモック (wiremock ベース)。
//!
//! シナリオ:
//! - ParameterProposal: 週次バッチ用パラメータ提案 JSON
//! - SwingSignal: SwingLLM 用シグナル提案 JSON
//! - NoTrade: SwingLLM "no_trade" レスポンス
//! - InvalidResponse: 不正レスポンス
//! - Timeout: レスポンス遅延

use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub enum GeminiScenario {
    ParameterProposal {
        entry_period: u32,
        exit_period: u32,
        reasoning: String,
    },
    SwingSignal {
        direction: String,
        confidence: f64,
    },
    NoTrade,
    InvalidResponse,
    Timeout {
        delay_ms: u64,
    },
}

pub struct MockGemini {
    server: MockServer,
}

impl MockGemini {
    pub async fn start(scenario: GeminiScenario) -> Self {
        let server = MockServer::start().await;

        match scenario {
            GeminiScenario::ParameterProposal {
                entry_period,
                exit_period,
                reasoning,
            } => {
                let proposal_json = serde_json::json!({
                    "entry_period": entry_period,
                    "exit_period": exit_period,
                    "reasoning": reasoning
                });
                let body = make_gemini_response(&proposal_json.to_string());
                Mock::given(method("POST"))
                    .and(path_regex(r"/v1beta/models/.+:generateContent"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                    .mount(&server)
                    .await;
            }
            GeminiScenario::SwingSignal {
                direction,
                confidence,
            } => {
                let signal_json = serde_json::json!({
                    "direction": direction,
                    "confidence": confidence,
                    "reasoning": "Mock analysis suggests trend continuation"
                });
                let body = make_gemini_response(&signal_json.to_string());
                Mock::given(method("POST"))
                    .and(path_regex(r"/v1beta/models/.+:generateContent"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                    .mount(&server)
                    .await;
            }
            GeminiScenario::NoTrade => {
                let signal_json = serde_json::json!({
                    "direction": "no_trade",
                    "confidence": 0.0,
                    "reasoning": "No clear signal detected"
                });
                let body = make_gemini_response(&signal_json.to_string());
                Mock::given(method("POST"))
                    .and(path_regex(r"/v1beta/models/.+:generateContent"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                    .mount(&server)
                    .await;
            }
            GeminiScenario::InvalidResponse => {
                let body =
                    make_gemini_response("this is not valid json {{{ broken content <<<");
                Mock::given(method("POST"))
                    .and(path_regex(r"/v1beta/models/.+:generateContent"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                    .mount(&server)
                    .await;
            }
            GeminiScenario::Timeout { delay_ms } => {
                let body = make_gemini_response("{}");
                Mock::given(method("POST"))
                    .and(path_regex(r"/v1beta/models/.+:generateContent"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(&body)
                            .set_delay(std::time::Duration::from_millis(delay_ms)),
                    )
                    .mount(&server)
                    .await;
            }
        }

        Self { server }
    }

    pub fn base_url(&self) -> String {
        self.server.uri()
    }
}

/// Gemini API のレスポンスフォーマットを組み立てる。
fn make_gemini_response(text: &str) -> serde_json::Value {
    serde_json::json!({
        "candidates": [{
            "content": {
                "parts": [{
                    "text": text
                }],
                "role": "model"
            },
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": {
            "promptTokenCount": 100,
            "candidatesTokenCount": 50,
            "totalTokenCount": 150
        }
    })
}
```

```bash
cargo test -p auto-trader-integration-tests --test smoke_test -- mock_gemini 2>&1 | tail -20
```

**Commit:** `feat(test): add MockGemini with parameter/signal/error scenarios`

---

## Task 12: Final smoke test (DB + fixture + mock + assert)

**Files:**
- Create: `crates/integration-tests/tests/smoke_test.rs` (final version)

- [ ] **Step 1: Write the integrated smoke test**

Replace `crates/integration-tests/tests/smoke_test.rs` with the final version that brings everything together. The test must: start DB, load fixture, create mock, run a basic assertion, and demonstrate failure output on intentional mismatch.

```rust
//! Smoke test: テスト基盤全体 (DB + fixture + mocks + failure output) の統合動作確認。

use auto_trader_core::types::{Direction, Exchange, Pair};
use auto_trader_integration_tests::helpers::{db, failure_output, fixture_loader};
use auto_trader_integration_tests::mocks::{
    bitflyer_ws::{BitflyerWsScenario, MockBitflyerWs},
    exchange_api::MockExchangeApi,
    gemini::{GeminiScenario, MockGemini},
    gmo_fx_server::{GmoFxScenario, MockGmoFxServer},
    oanda_server::{MockOandaServer, OandaScenario},
    slack_webhook::MockSlackWebhook,
    vegapunk::MockVegapunk,
};
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_notify::{Notifier, NotifyEvent, OrderFilledEvent};
use chrono::Utc;
use failure_output::{FailureContext, TracingCapture};
use rust_decimal_macros::dec;

// -----------------------------------------------------------------------
// DB helper tests
// -----------------------------------------------------------------------

#[sqlx::test(migrations = "../../migrations")]
async fn db_helper_snapshot_returns_table_contents(pool: sqlx::PgPool) {
    sqlx::query(
        r#"INSERT INTO trading_accounts
               (id, name, account_type, exchange, strategy,
                initial_balance, current_balance, leverage, currency)
           VALUES (gen_random_uuid(), 'smoke', 'paper', 'gmo_fx', 'bb_mean_revert_v1',
                   100000, 100000, 2, 'JPY')"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let snapshot = db::snapshot_tables(&pool, &["trading_accounts"]).await;
    assert!(
        snapshot.contains("smoke"),
        "snapshot must contain seeded account name: {snapshot}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn db_seed_standard_accounts(pool: sqlx::PgPool) {
    let accounts = db::seed_standard_accounts(&pool).await;

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM trading_accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 2, "must seed 2 accounts");

    // Verify both IDs exist
    let bf: Option<(String,)> =
        sqlx::query_as("SELECT name FROM trading_accounts WHERE id = $1")
            .bind(accounts.bitflyer_id)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(bf.is_some(), "bitflyer account must exist");

    let gmo: Option<(String,)> =
        sqlx::query_as("SELECT name FROM trading_accounts WHERE id = $1")
            .bind(accounts.gmo_fx_id)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(gmo.is_some(), "gmo_fx account must exist");
}

// -----------------------------------------------------------------------
// Fixture loader tests
// -----------------------------------------------------------------------

#[sqlx::test(migrations = "../../migrations")]
async fn fixture_loader_inserts_candles(pool: sqlx::PgPool) {
    let count =
        fixture_loader::load_price_candles(&pool, "smoke_test.csv", "USD_JPY", "M5", "oanda")
            .await
            .unwrap();
    assert!(count > 0, "must insert at least 1 candle");

    let db_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM price_candles WHERE pair = 'USD_JPY'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(db_count.0, count as i64);
}

// -----------------------------------------------------------------------
// Failure output tests
// -----------------------------------------------------------------------

#[sqlx::test(migrations = "../../migrations")]
async fn failure_output_contains_all_sections(pool: sqlx::PgPool) {
    let ctx = FailureContext {
        test_name: "smoke::failure_output_test",
        source_file: file!(),
        source_line: line!(),
        fixture: Some("smoke_test.csv"),
        expected: "1 open trade",
        actual: "0 trades",
    };

    let logs = vec![
        "INFO  strategy warmup: loaded 5 candles".to_string(),
        "WARN  freshness gate rejected".to_string(),
    ];

    let db_snapshot = db::snapshot_tables(&pool, &["trading_accounts", "trades"]).await;
    let output = failure_output::format_failure(&ctx, &logs, &db_snapshot);

    assert!(output.contains("[FAIL]"), "must contain [FAIL] header");
    assert!(
        output.contains("smoke::failure_output_test"),
        "must contain test name"
    );
    assert!(
        output.contains("smoke_test.csv"),
        "must contain fixture name"
    );
    assert!(output.contains("expected:"), "must contain expected");
    assert!(output.contains("actual:"), "must contain actual");
    assert!(
        output.contains("=== application log ==="),
        "must contain log section"
    );
    assert!(
        output.contains("freshness gate rejected"),
        "must contain log content"
    );
    assert!(
        output.contains("=== db state ==="),
        "must contain db state section"
    );
    assert!(
        output.contains("=== git diff"),
        "must contain git diff section"
    );
}

// -----------------------------------------------------------------------
// Tracing capture tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn tracing_capture_records_events() {
    let (capture, _guard) = TracingCapture::init();

    tracing::info!("test message one");
    tracing::warn!("test warning two");

    let logs = capture.drain();
    assert!(logs.len() >= 2, "must capture at least 2 log events");
    assert!(
        logs.iter().any(|l| l.contains("test message one")),
        "must contain info message"
    );
    assert!(
        logs.iter().any(|l| l.contains("test warning two")),
        "must contain warn message"
    );
}

// -----------------------------------------------------------------------
// MockExchangeApi tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn mock_exchange_api_configurable_responses() {
    let mock = MockExchangeApi::builder()
        .with_positions(
            "FX_BTC_JPY",
            vec![auto_trader_market::bitflyer_private::ExchangePosition {
                product_code: "FX_BTC_JPY".to_string(),
                side: "BUY".to_string(),
                price: dec!(11_500_000),
                size: dec!(0.001),
                commission: dec!(0),
                swap_point_accumulate: dec!(0),
                require_collateral: dec!(0),
                open_date: "2026-01-01T00:00:00".to_string(),
                leverage: dec!(2),
                pnl: dec!(0),
                sfd: dec!(0),
            }],
        )
        .with_collateral(auto_trader_market::bitflyer_private::Collateral {
            collateral: dec!(100_000),
            open_position_pnl: dec!(0),
            require_collateral: dec!(50_000),
            keep_rate: dec!(2.0),
        })
        .build();

    let positions = mock.get_positions("FX_BTC_JPY").await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].size, dec!(0.001));

    let collateral = mock.get_collateral().await.unwrap();
    assert_eq!(collateral.collateral, dec!(100_000));
}

#[tokio::test]
async fn mock_exchange_api_failure_injection() {
    let mock = MockExchangeApi::builder()
        .with_get_positions_failures(2)
        .build();

    assert!(mock.get_positions("FX_BTC_JPY").await.is_err());
    assert!(mock.get_positions("FX_BTC_JPY").await.is_err());
    let positions = mock.get_positions("FX_BTC_JPY").await.unwrap();
    assert!(positions.is_empty());
}

#[tokio::test]
async fn mock_exchange_api_call_counting() {
    let mock = MockExchangeApi::builder().build();
    let _ = mock.get_positions("A").await;
    let _ = mock.get_positions("B").await;
    let _ = mock.get_collateral().await;

    assert_eq!(
        mock.call_counts
            .get_positions
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert_eq!(
        mock.call_counts
            .get_collateral
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

// -----------------------------------------------------------------------
// MockGmoFxServer tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn mock_gmo_fx_server_normal_ticker() {
    let server = MockGmoFxServer::start(GmoFxScenario::NormalTicker {
        symbol: "USD_JPY".to_string(),
        bid: "150.100".to_string(),
        ask: "150.200".to_string(),
    })
    .await;

    let resp = reqwest::get(format!("{}/public/v1/ticker", server.base_url()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 0);
    assert_eq!(body["data"][0]["symbol"], "USD_JPY");
}

#[tokio::test]
async fn mock_gmo_fx_server_maintenance() {
    let server = MockGmoFxServer::start(GmoFxScenario::Maintenance).await;

    let resp = reqwest::get(format!("{}/public/v1/ticker", server.base_url()))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 5);
    assert!(body["data"].as_array().unwrap().is_empty());
}

// -----------------------------------------------------------------------
// MockBitflyerWs tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn mock_bitflyer_ws_sends_ticks() {
    let server = MockBitflyerWs::start(BitflyerWsScenario::NormalTicks {
        product_code: "FX_BTC_JPY".to_string(),
        ticks: vec![
            ("11500000", "11500100", "11499900"),
            ("11500200", "11500300", "11500100"),
        ],
    })
    .await;

    let (mut ws, _) = tokio_tungstenite::connect_async(&server.ws_url())
        .await
        .unwrap();

    let subscribe = serde_json::json!({
        "method": "subscribe",
        "params": { "channel": "lightning_ticker_FX_BTC_JPY" }
    });
    use futures_util::SinkExt;
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    ws.send(Message::Text(subscribe.to_string())).await.unwrap();

    let msg1 = ws.next().await.unwrap().unwrap();
    assert!(msg1.is_text(), "first message must be text");
    let parsed: serde_json::Value = serde_json::from_str(&msg1.to_text().unwrap()).unwrap();
    assert_eq!(parsed["params"]["message"]["product_code"], "FX_BTC_JPY");

    let msg2 = ws.next().await.unwrap().unwrap();
    assert!(msg2.is_text(), "second message must be text");
}

// -----------------------------------------------------------------------
// MockOandaServer tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn mock_oanda_server_normal_candles() {
    let server = MockOandaServer::start(OandaScenario::NormalCandles {
        instrument: "USD_JPY".to_string(),
        candles: vec![
            ("150.100", "150.200", "150.050", "150.150", true),
            ("150.150", "150.300", "150.100", "150.250", true),
        ],
    })
    .await;

    let resp = reqwest::Client::new()
        .get(format!(
            "{}/v3/accounts/test/instruments/USD_JPY/candles",
            server.base_url()
        ))
        .header("Authorization", "Bearer test-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["candles"].as_array().unwrap().len(), 2);
}

// -----------------------------------------------------------------------
// MockSlackWebhook tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn mock_slack_webhook_captures_body() {
    let mock = MockSlackWebhook::start().await;

    let notifier = Notifier::new(Some(mock.webhook_url()));
    let event = NotifyEvent::OrderFilled(OrderFilledEvent {
        account_name: "テスト口座".to_string(),
        exchange: Exchange::BitflyerCfd,
        trade_id: uuid::Uuid::nil(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        quantity: dec!(0.01),
        price: dec!(11_500_000),
        at: Utc::now(),
    });
    notifier.send(event).await.unwrap();

    let bodies = mock.received_bodies().await;
    assert_eq!(bodies.len(), 1, "must capture exactly 1 webhook call");
    assert!(
        bodies[0].contains("約定"),
        "body must contain order filled text"
    );
}

#[tokio::test]
async fn mock_slack_webhook_error_scenario() {
    let mock = MockSlackWebhook::start_with_status(500).await;

    let notifier = Notifier::new(Some(mock.webhook_url()));
    let event = NotifyEvent::OrderFailed(auto_trader_notify::OrderFailedEvent {
        account_name: "テスト口座".to_string(),
        exchange: Exchange::BitflyerCfd,
        strategy_name: "test".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        reason: "test error".to_string(),
    });
    let result = notifier.send(event).await;
    assert!(result.is_err(), "must return error on 500");
}

// -----------------------------------------------------------------------
// MockVegapunk tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn mock_vegapunk_ingest_and_search() {
    let mock = MockVegapunk::start().await;

    let mut client =
        auto_trader_vegapunk::client::VegapunkClient::connect(&mock.endpoint(), "test-schema", None)
            .await
            .unwrap();

    let ingest_resp = client
        .ingest_raw(
            "test content",
            "integration_test",
            "test-channel",
            "2026-01-01T00:00:00Z",
        )
        .await
        .unwrap();
    assert_eq!(ingest_resp.chunk_count, 1);

    let search_resp = client.search("test query", "local", 10).await.unwrap();
    assert!(!search_resp.search_id.is_empty());

    client
        .feedback(&search_resp.search_id, 5, "good")
        .await
        .unwrap();
    client.merge().await.unwrap();

    assert_eq!(
        mock.state
            .ingest_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        mock.state
            .search_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

// -----------------------------------------------------------------------
// MockGemini tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn mock_gemini_parameter_proposal() {
    let server = MockGemini::start(GeminiScenario::ParameterProposal {
        entry_period: 15,
        exit_period: 8,
        reasoning: "Volatility suggests tighter channels".to_string(),
    })
    .await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/v1beta/models/gemini-2.5-flash:generateContent",
            server.base_url()
        ))
        .header("x-goog-api-key", "test-key")
        .json(&serde_json::json!({"contents": []}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let text = body["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("entry_period"));
}

#[tokio::test]
async fn mock_gemini_invalid_response() {
    let server = MockGemini::start(GeminiScenario::InvalidResponse).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "{}/v1beta/models/gemini-2.5-flash:generateContent",
            server.base_url()
        ))
        .header("x-goog-api-key", "test-key")
        .json(&serde_json::json!({"contents": []}))
        .send()
        .await
        .unwrap();

    let body: serde_json::Value = resp.json().await.unwrap();
    let text = body["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(text).is_err());
}

// -----------------------------------------------------------------------
// Integrated smoke test: DB + fixture + mock + tracing + failure output
// -----------------------------------------------------------------------

#[sqlx::test(migrations = "../../migrations")]
async fn smoke_full_infra_integration(pool: sqlx::PgPool) {
    // 1. Tracing capture
    let (capture, _guard) = TracingCapture::init();

    // 2. Seed DB
    let accounts = db::seed_standard_accounts(&pool).await;
    tracing::info!(
        bitflyer_id = %accounts.bitflyer_id,
        gmo_fx_id = %accounts.gmo_fx_id,
        "seeded standard accounts"
    );

    // 3. Load fixture
    let candle_count =
        fixture_loader::load_price_candles(&pool, "smoke_test.csv", "USD_JPY", "M5", "oanda")
            .await
            .unwrap();
    tracing::info!(candle_count, "loaded fixture candles");
    assert_eq!(candle_count, 5, "smoke_test.csv has 5 rows");

    // 4. Verify DB state
    let db_candle_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM price_candles WHERE pair = 'USD_JPY'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(db_candle_count.0, 5);

    // 5. Use MockExchangeApi
    let mock_api = MockExchangeApi::builder().build();
    let positions = mock_api.get_positions("FX_BTC_JPY").await.unwrap();
    assert!(positions.is_empty(), "no positions configured → empty");
    tracing::info!("MockExchangeApi works");

    // 6. Verify tracing capture
    let logs = capture.drain();
    assert!(
        logs.iter().any(|l| l.contains("seeded standard accounts")),
        "captured logs must contain our info message"
    );

    // 7. Verify failure output formatting works (without actually failing)
    let db_snapshot =
        db::snapshot_tables(&pool, &["trading_accounts", "price_candles"]).await;
    let ctx = FailureContext {
        test_name: "smoke::full_infra_integration",
        source_file: file!(),
        source_line: line!(),
        fixture: Some("smoke_test.csv"),
        expected: "5 candles loaded, 2 accounts seeded",
        actual: "5 candles loaded, 2 accounts seeded",
    };
    let output = failure_output::format_failure(&ctx, &logs, &db_snapshot);
    assert!(
        output.contains("smoke::full_infra_integration"),
        "failure output must be properly formatted"
    );
    assert!(
        output.contains("trading_accounts"),
        "failure output must contain DB snapshot"
    );
}
```

- [ ] **Step 2: Run all smoke tests**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
export DATABASE_URL="postgresql://auto-trader:auto-trader@localhost:15432/auto_trader"
cargo test -p auto-trader-integration-tests --test smoke_test 2>&1 | tail -30
```

All tests must pass. Expected output: 15+ tests passing.

**Commit:** `feat(test): add integrated smoke test proving infra works`

---

## Run Commands Summary

```bash
# Check compilation
cargo check -p auto-trader-integration-tests

# Run all smoke tests (requires DB)
export DATABASE_URL="postgresql://auto-trader:auto-trader@localhost:15432/auto_trader"
docker compose up -d db
cargo test -p auto-trader-integration-tests --test smoke_test

# Run specific test
cargo test -p auto-trader-integration-tests --test smoke_test -- smoke_full_infra_integration

# Future: Run all integration tests
cargo test -p auto-trader-integration-tests
```
