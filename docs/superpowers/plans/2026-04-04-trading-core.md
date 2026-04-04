# Trading Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** OANDA デモ API から価格を取得し、テクニカル指標ベースの戦略でペーパートレードを実行し、結果を PostgreSQL に記録する動作するボットを構築する。

**Architecture:** tokio の mpsc channel によるイベント駆動アーキテクチャ。market-monitor が PriceEvent を発行し、strategy-engine が各戦略に配信、Signal を executor に渡してトレードを実行する。全コンポーネントは単一バイナリ内の tokio タスクとして動作する。

**Tech Stack:** Rust (2024 edition), tokio, sqlx (PostgreSQL), reqwest (OANDA API), rust_decimal, chrono, uuid, serde, toml, tracing

**Plan:** 1 of 3 (Trading Core -> Intelligence Layer -> Dashboard)

---

## File Structure

```
auto-trader/
  Cargo.toml                          # workspace
  crates/
    core/
      Cargo.toml
      src/
        lib.rs                        # re-exports
        config.rs                     # AppConfig, TOML loading
        event.rs                      # PriceEvent, SignalEvent, TradeEvent
        types.rs                      # Pair, Direction, Signal, Trade, Position, Candle
        strategy.rs                   # Strategy trait
        executor.rs                   # OrderExecutor trait
    db/
      Cargo.toml
      src/
        lib.rs
        pool.rs                       # create_pool()
        trades.rs                     # trades CRUD
        candles.rs                    # price_candles CRUD
        summary.rs                    # daily_summary upsert
    market/
      Cargo.toml
      src/
        lib.rs
        oanda.rs                      # OandaClient (price fetch, candle fetch)
        monitor.rs                    # MarketMonitor (poll loop, PriceEvent emit)
        indicators.rs                 # SMA, EMA, RSI
    strategy/
      Cargo.toml
      src/
        lib.rs
        engine.rs                     # StrategyEngine (dispatch, conflict rules)
        trend_follow.rs               # TrendFollowV1 strategy
    executor/
      Cargo.toml
      src/
        lib.rs
        paper.rs                      # PaperTrader (virtual balance, positions)
    app/
      Cargo.toml
      src/
        main.rs                       # tokio tasks, channel wiring, graceful shutdown
  migrations/
    20260404000001_initial.sql        # all tables
  config/
    default.toml                      # default config
  docker-compose.yml
  Dockerfile
  .env.example
```

---

### Task 1: Workspace Scaffolding

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/core/Cargo.toml`
- Create: `crates/core/src/lib.rs`
- Create: `crates/db/Cargo.toml`
- Create: `crates/db/src/lib.rs`
- Create: `crates/market/Cargo.toml`
- Create: `crates/market/src/lib.rs`
- Create: `crates/strategy/Cargo.toml`
- Create: `crates/strategy/src/lib.rs`
- Create: `crates/executor/Cargo.toml`
- Create: `crates/executor/src/lib.rs`
- Create: `crates/app/Cargo.toml`
- Create: `crates/app/src/main.rs`
- Create: `rust-toolchain.toml`

- [ ] **Step 1: Create workspace root Cargo.toml**

```toml
[workspace]
resolver = "2"
members = [
    "crates/core",
    "crates/db",
    "crates/market",
    "crates/strategy",
    "crates/executor",
    "crates/app",
]

[workspace.package]
edition = "2024"
rust-version = "1.85"
license = "MIT"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4", "serde"] }
rust_decimal = { version = "1", features = ["serde-with-str"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow = "1"
thiserror = "2"
reqwest = { version = "0.12", features = ["json"] }
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "uuid", "chrono", "rust_decimal"] }

# internal
auto-trader-core = { path = "crates/core" }
auto-trader-db = { path = "crates/db" }
auto-trader-market = { path = "crates/market" }
auto-trader-strategy = { path = "crates/strategy" }
auto-trader-executor = { path = "crates/executor" }
```

- [ ] **Step 2: Create rust-toolchain.toml**

```toml
[toolchain]
channel = "stable"
```

- [ ] **Step 3: Create core crate**

`crates/core/Cargo.toml`:
```toml
[package]
name = "auto-trader-core"
version = "0.1.0"
edition.workspace = true

[dependencies]
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
toml = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
rust_decimal = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }
```

`crates/core/src/lib.rs`:
```rust
pub mod config;
pub mod event;
pub mod executor;
pub mod strategy;
pub mod types;
```

Create empty module files: `config.rs`, `event.rs`, `executor.rs`, `strategy.rs`, `types.rs` (each with just a comment `// TODO: implement in Task 2`).

- [ ] **Step 4: Create remaining crate stubs**

`crates/db/Cargo.toml`:
```toml
[package]
name = "auto-trader-db"
version = "0.1.0"
edition.workspace = true

[dependencies]
auto-trader-core = { workspace = true }
sqlx = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
rust_decimal = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
```

`crates/market/Cargo.toml`:
```toml
[package]
name = "auto-trader-market"
version = "0.1.0"
edition.workspace = true

[dependencies]
auto-trader-core = { workspace = true }
reqwest = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
rust_decimal = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
```

`crates/strategy/Cargo.toml`:
```toml
[package]
name = "auto-trader-strategy"
version = "0.1.0"
edition.workspace = true

[dependencies]
auto-trader-core = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
rust_decimal = { workspace = true }
```

`crates/executor/Cargo.toml`:
```toml
[package]
name = "auto-trader-executor"
version = "0.1.0"
edition.workspace = true

[dependencies]
auto-trader-core = { workspace = true }
tokio = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
rust_decimal = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }
```

`crates/app/Cargo.toml`:
```toml
[package]
name = "auto-trader"
version = "0.1.0"
edition.workspace = true

[dependencies]
auto-trader-core = { workspace = true }
auto-trader-db = { workspace = true }
auto-trader-market = { workspace = true }
auto-trader-strategy = { workspace = true }
auto-trader-executor = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow = { workspace = true }
```

`crates/app/src/main.rs`:
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tracing::info!("auto-trader starting");
    Ok(())
}
```

All other `lib.rs` files: empty (just `// TODO`).

- [ ] **Step 5: Verify workspace compiles**

Run: `cargo build`
Expected: compiles with no errors.

- [ ] **Step 6: Commit**

```bash
git checkout -b feat/trading-core && git add -A && git commit -m "feat: scaffold workspace with core, db, market, strategy, executor, app crates"
```

---

### Task 2: Core Types and Events

**Files:**
- Create: `crates/core/src/types.rs`
- Create: `crates/core/src/event.rs`

- [ ] **Step 1: Write tests for core types**

Add to `crates/core/src/types.rs`:
```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pair(pub String);

impl Pair {
    pub fn new(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Long,
    Short,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeMode {
    Live,
    Paper,
    Backtest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    TpHit,
    SlHit,
    Manual,
    SignalReverse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: Uuid,
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_pips: Option<Decimal>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<ExitReason>,
    pub mode: TradeMode,
    pub status: TradeStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub trade: Trade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    pub pair: Pair,
    pub timeframe: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Option<i32>,
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn pair_display_format() {
        let pair = Pair::new("USD_JPY");
        assert_eq!(pair.to_string(), "USD_JPY");
    }

    #[test]
    fn direction_serializes_snake_case() {
        let json = serde_json::to_string(&Direction::Long).unwrap();
        assert_eq!(json, r#""long""#);
    }

    #[test]
    fn signal_roundtrip() {
        let signal = Signal {
            strategy_name: "test".to_string(),
            pair: Pair::new("USD_JPY"),
            direction: Direction::Long,
            entry_price: dec!(150.00),
            stop_loss: dec!(149.50),
            take_profit: dec!(151.00),
            confidence: 0.8,
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&signal).unwrap();
        let back: Signal = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pair, signal.pair);
        assert_eq!(back.direction, Direction::Long);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p auto-trader-core`
Expected: 3 tests pass.

- [ ] **Step 3: Write event types**

`crates/core/src/event.rs`:
```rust
use crate::types::{Candle, ExitReason, Pair, Signal, Trade};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PriceEvent {
    pub pair: Pair,
    pub candle: Candle,
    pub indicators: HashMap<String, Decimal>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SignalEvent {
    pub signal: Signal,
}

#[derive(Debug, Clone)]
pub enum TradeAction {
    Opened,
    Closed { exit_price: Decimal, exit_reason: ExitReason },
}

#[derive(Debug, Clone)]
pub struct TradeEvent {
    pub trade: Trade,
    pub action: TradeAction,
}
```

- [ ] **Step 4: Update lib.rs re-exports**

`crates/core/src/lib.rs`:
```rust
pub mod config;
pub mod event;
pub mod executor;
pub mod strategy;
pub mod types;
```

- [ ] **Step 5: Add rust_decimal_macros to core dev-dependencies**

Add to `crates/core/Cargo.toml`:
```toml
[dev-dependencies]
rust_decimal_macros = "1"
```

- [ ] **Step 6: Verify all tests pass**

Run: `cargo test -p auto-trader-core`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/types.rs crates/core/src/event.rs crates/core/src/lib.rs crates/core/Cargo.toml && git commit -m "feat(core): add types (Pair, Signal, Trade, Candle) and events (PriceEvent, SignalEvent, TradeEvent)"
```

---

### Task 3: Config Loading

**Files:**
- Create: `crates/core/src/config.rs`
- Create: `config/default.toml`

- [ ] **Step 1: Write config types with tests**

`crates/core/src/config.rs`:
```rust
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub oanda: OandaConfig,
    pub vegapunk: VegapunkConfig,
    pub database: DatabaseConfig,
    pub monitor: MonitorConfig,
    pub pairs: PairsConfig,
    #[serde(default)]
    pub strategies: Vec<StrategyConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OandaConfig {
    pub api_url: String,
    pub account_id: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct VegapunkConfig {
    pub endpoint: String,
    pub schema: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MonitorConfig {
    pub interval_secs: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PairsConfig {
    pub active: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StrategyConfig {
    pub name: String,
    pub enabled: bool,
    pub mode: String,
    pub pairs: Vec<String>,
    #[serde(default)]
    pub params: HashMap<String, toml::Value>,
}

impl AppConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"
[oanda]
api_url = "https://api-fxpractice.oanda.com"
account_id = "101-001-12345678-001"

[vegapunk]
endpoint = "http://fuj11-agent-01:3000"
schema = "fx-trading"

[database]
url = "postgresql://user:pass@localhost:5432/auto_trader"

[monitor]
interval_secs = 60

[pairs]
active = ["USD_JPY"]

[[strategies]]
name = "trend_follow_v1"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]
params = { ma_short = 20, ma_long = 50 }
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.oanda.api_url, "https://api-fxpractice.oanda.com");
        assert_eq!(config.strategies.len(), 1);
        assert_eq!(config.strategies[0].name, "trend_follow_v1");
        assert_eq!(config.pairs.active, vec!["USD_JPY"]);
    }
}
```

- [ ] **Step 2: Run test**

Run: `cargo test -p auto-trader-core config`
Expected: 1 test passes.

- [ ] **Step 3: Create default config file**

`config/default.toml`:
```toml
[oanda]
api_url = "https://api-fxpractice.oanda.com"
account_id = ""  # set via OANDA_ACCOUNT_ID env var

[vegapunk]
endpoint = "http://fuj11-agent-01:3000"
schema = "fx-trading"

[database]
url = "postgresql://auto-trader:auto-trader@db:5432/auto_trader"

[monitor]
interval_secs = 60

[pairs]
active = ["USD_JPY", "EUR_USD"]

[[strategies]]
name = "trend_follow_v1"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]
params = { ma_short = 20, ma_long = 50, rsi_threshold = 70 }
```

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/config.rs config/default.toml && git commit -m "feat(core): add TOML config loading with AppConfig"
```

---

### Task 4: Strategy and Executor Traits

**Files:**
- Create: `crates/core/src/strategy.rs`
- Create: `crates/core/src/executor.rs`

- [ ] **Step 1: Define Strategy trait**

`crates/core/src/strategy.rs`:
```rust
use crate::event::PriceEvent;
use crate::types::Signal;

pub struct MacroUpdate {
    pub summary: String,
    pub adjustments: std::collections::HashMap<String, String>,
}

pub trait Strategy: Send + 'static {
    fn name(&self) -> &str;
    fn on_price(
        &mut self,
        event: &PriceEvent,
    ) -> impl std::future::Future<Output = Option<Signal>> + Send;
    fn on_macro_update(&mut self, update: &MacroUpdate);
}
```

- [ ] **Step 2: Define OrderExecutor trait**

`crates/core/src/executor.rs`:
```rust
use crate::types::{Position, Signal, Trade};

pub trait OrderExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        signal: &Signal,
    ) -> impl std::future::Future<Output = anyhow::Result<Trade>> + Send;
    fn open_positions(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<Position>>> + Send;
    fn close_position(
        &self,
        id: &str,
        exit_reason: crate::types::ExitReason,
        exit_price: rust_decimal::Decimal,
    ) -> impl std::future::Future<Output = anyhow::Result<Trade>> + Send;
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo build -p auto-trader-core`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/strategy.rs crates/core/src/executor.rs && git commit -m "feat(core): add Strategy and OrderExecutor traits"
```

---

### Task 5: Database Layer

**Files:**
- Create: `migrations/20260404000001_initial.sql`
- Create: `crates/db/src/lib.rs`
- Create: `crates/db/src/pool.rs`
- Create: `crates/db/src/trades.rs`
- Create: `crates/db/src/candles.rs`
- Create: `crates/db/src/summary.rs`

- [ ] **Step 1: Create migration**

`migrations/20260404000001_initial.sql`:
```sql
CREATE TABLE trades (
    id UUID PRIMARY KEY,
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    direction TEXT NOT NULL,
    entry_price DECIMAL NOT NULL,
    exit_price DECIMAL,
    stop_loss DECIMAL NOT NULL,
    take_profit DECIMAL NOT NULL,
    entry_at TIMESTAMPTZ NOT NULL,
    exit_at TIMESTAMPTZ,
    pnl_pips DECIMAL,
    pnl_amount DECIMAL,
    exit_reason TEXT,
    mode TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_trades_strategy ON trades (strategy_name);
CREATE INDEX idx_trades_pair ON trades (pair);
CREATE INDEX idx_trades_mode ON trades (mode);
CREATE INDEX idx_trades_status ON trades (status);
CREATE INDEX idx_trades_entry_at ON trades (entry_at);

CREATE TABLE price_candles (
    id BIGSERIAL PRIMARY KEY,
    pair TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    open DECIMAL NOT NULL,
    high DECIMAL NOT NULL,
    low DECIMAL NOT NULL,
    close DECIMAL NOT NULL,
    volume INTEGER,
    timestamp TIMESTAMPTZ NOT NULL,
    UNIQUE (pair, timeframe, timestamp)
);

CREATE INDEX idx_candles_pair_tf ON price_candles (pair, timeframe);
CREATE INDEX idx_candles_timestamp ON price_candles (timestamp);

CREATE TABLE strategy_configs (
    id UUID PRIMARY KEY,
    strategy_name TEXT NOT NULL,
    version TEXT NOT NULL,
    params JSONB NOT NULL,
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE daily_summary (
    id BIGSERIAL PRIMARY KEY,
    date DATE NOT NULL,
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    mode TEXT NOT NULL,
    trade_count INTEGER NOT NULL DEFAULT 0,
    win_count INTEGER NOT NULL DEFAULT 0,
    total_pnl DECIMAL NOT NULL DEFAULT 0,
    max_drawdown DECIMAL NOT NULL DEFAULT 0,
    UNIQUE (date, strategy_name, pair, mode)
);

CREATE TABLE macro_events (
    id UUID PRIMARY KEY,
    summary TEXT NOT NULL,
    event_type TEXT NOT NULL,
    impact TEXT NOT NULL,
    event_at TIMESTAMPTZ NOT NULL,
    source TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_macro_events_type ON macro_events (event_type);
CREATE INDEX idx_macro_events_at ON macro_events (event_at);
```

- [ ] **Step 2: Write pool creation**

`crates/db/src/pool.rs`:
```rust
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub async fn create_pool(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    Ok(pool)
}
```

- [ ] **Step 3: Write trades CRUD**

`crates/db/src/trades.rs`:
```rust
use auto_trader_core::types::{
    Candle, Direction, ExitReason, Pair, Trade, TradeMode, TradeStatus,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn insert_trade(pool: &PgPool, trade: &Trade) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO trades
           (id, strategy_name, pair, direction, entry_price, exit_price,
            stop_loss, take_profit, entry_at, exit_at, pnl_pips, pnl_amount,
            exit_reason, mode, status)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)"#,
    )
    .bind(trade.id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(serde_json::to_string(&trade.direction)?.trim_matches('"'))
    .bind(trade.entry_price)
    .bind(trade.exit_price)
    .bind(trade.stop_loss)
    .bind(trade.take_profit)
    .bind(trade.entry_at)
    .bind(trade.exit_at)
    .bind(trade.pnl_pips)
    .bind(trade.pnl_amount)
    .bind(trade.exit_reason.map(|r| {
        serde_json::to_string(&r)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string()
    }))
    .bind(serde_json::to_string(&trade.mode).unwrap_or_default().trim_matches('"'))
    .bind(serde_json::to_string(&trade.status).unwrap_or_default().trim_matches('"'))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_trade_closed(
    pool: &PgPool,
    id: Uuid,
    exit_price: Decimal,
    exit_at: DateTime<Utc>,
    pnl_pips: Decimal,
    pnl_amount: Decimal,
    exit_reason: ExitReason,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"UPDATE trades
           SET exit_price = $2, exit_at = $3, pnl_pips = $4, pnl_amount = $5,
               exit_reason = $6, status = 'closed'
           WHERE id = $1"#,
    )
    .bind(id)
    .bind(exit_price)
    .bind(exit_at)
    .bind(pnl_pips)
    .bind(pnl_amount)
    .bind(serde_json::to_string(&exit_reason).unwrap_or_default().trim_matches('"'))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_open_trades(
    pool: &PgPool,
    strategy_name: &str,
    pair: &str,
) -> anyhow::Result<Vec<Trade>> {
    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, direction, entry_price, exit_price,
                  stop_loss, take_profit, entry_at, exit_at, pnl_pips, pnl_amount,
                  exit_reason, mode, status, created_at
           FROM trades
           WHERE strategy_name = $1 AND pair = $2 AND status = 'open'"#,
    )
    .bind(strategy_name)
    .bind(pair)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(|r| r.try_into()).collect()
}

#[derive(sqlx::FromRow)]
struct TradeRow {
    id: Uuid,
    strategy_name: String,
    pair: String,
    direction: String,
    entry_price: Decimal,
    exit_price: Option<Decimal>,
    stop_loss: Decimal,
    take_profit: Decimal,
    entry_at: DateTime<Utc>,
    exit_at: Option<DateTime<Utc>>,
    pnl_pips: Option<Decimal>,
    pnl_amount: Option<Decimal>,
    exit_reason: Option<String>,
    mode: String,
    status: String,
    created_at: DateTime<Utc>,
}

impl TryFrom<TradeRow> for Trade {
    type Error = anyhow::Error;

    fn try_from(r: TradeRow) -> anyhow::Result<Self> {
        let direction = match r.direction.as_str() {
            "long" => Direction::Long,
            "short" => Direction::Short,
            other => anyhow::bail!("unknown direction: {other}"),
        };
        let mode = match r.mode.as_str() {
            "live" => TradeMode::Live,
            "paper" => TradeMode::Paper,
            "backtest" => TradeMode::Backtest,
            other => anyhow::bail!("unknown mode: {other}"),
        };
        let status = match r.status.as_str() {
            "open" => TradeStatus::Open,
            "closed" => TradeStatus::Closed,
            other => anyhow::bail!("unknown status: {other}"),
        };
        let exit_reason = r
            .exit_reason
            .as_deref()
            .map(|s| match s {
                "tp_hit" => Ok(ExitReason::TpHit),
                "sl_hit" => Ok(ExitReason::SlHit),
                "manual" => Ok(ExitReason::Manual),
                "signal_reverse" => Ok(ExitReason::SignalReverse),
                other => Err(anyhow::anyhow!("unknown exit_reason: {other}")),
            })
            .transpose()?;
        Ok(Trade {
            id: r.id,
            strategy_name: r.strategy_name,
            pair: Pair::new(&r.pair),
            direction,
            entry_price: r.entry_price,
            exit_price: r.exit_price,
            stop_loss: r.stop_loss,
            take_profit: r.take_profit,
            entry_at: r.entry_at,
            exit_at: r.exit_at,
            pnl_pips: r.pnl_pips,
            pnl_amount: r.pnl_amount,
            exit_reason,
            mode,
            status,
        })
    }
}
```

- [ ] **Step 4: Write candles CRUD**

`crates/db/src/candles.rs`:
```rust
use auto_trader_core::types::Candle;
use auto_trader_core::types::Pair;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;

pub async fn upsert_candle(pool: &PgPool, candle: &Candle) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO price_candles (pair, timeframe, open, high, low, close, volume, timestamp)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           ON CONFLICT (pair, timeframe, timestamp) DO UPDATE
           SET open = $3, high = $4, low = $5, close = $6, volume = $7"#,
    )
    .bind(&candle.pair.0)
    .bind(&candle.timeframe)
    .bind(candle.open)
    .bind(candle.high)
    .bind(candle.low)
    .bind(candle.close)
    .bind(candle.volume)
    .bind(candle.timestamp)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_candles(
    pool: &PgPool,
    pair: &str,
    timeframe: &str,
    limit: i64,
) -> anyhow::Result<Vec<Candle>> {
    let rows = sqlx::query_as::<_, CandleRow>(
        r#"SELECT pair, timeframe, open, high, low, close, volume, timestamp
           FROM price_candles
           WHERE pair = $1 AND timeframe = $2
           ORDER BY timestamp DESC
           LIMIT $3"#,
    )
    .bind(pair)
    .bind(timeframe)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.into()).collect())
}

#[derive(sqlx::FromRow)]
struct CandleRow {
    pair: String,
    timeframe: String,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: Option<i32>,
    timestamp: DateTime<Utc>,
}

impl From<CandleRow> for Candle {
    fn from(r: CandleRow) -> Self {
        Candle {
            pair: Pair::new(&r.pair),
            timeframe: r.timeframe,
            open: r.open,
            high: r.high,
            low: r.low,
            close: r.close,
            volume: r.volume,
            timestamp: r.timestamp,
        }
    }
}
```

- [ ] **Step 5: Write daily_summary upsert**

`crates/db/src/summary.rs`:
```rust
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;

pub async fn upsert_daily_summary(
    pool: &PgPool,
    date: NaiveDate,
    strategy_name: &str,
    pair: &str,
    mode: &str,
    trade_count_delta: i32,
    win_count_delta: i32,
    pnl_delta: Decimal,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO daily_summary (date, strategy_name, pair, mode, trade_count, win_count, total_pnl)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           ON CONFLICT (date, strategy_name, pair, mode) DO UPDATE
           SET trade_count = daily_summary.trade_count + $5,
               win_count = daily_summary.win_count + $6,
               total_pnl = daily_summary.total_pnl + $7"#,
    )
    .bind(date)
    .bind(strategy_name)
    .bind(pair)
    .bind(mode)
    .bind(trade_count_delta)
    .bind(win_count_delta)
    .bind(pnl_delta)
    .execute(pool)
    .await?;
    Ok(())
}
```

- [ ] **Step 6: Write lib.rs**

`crates/db/src/lib.rs`:
```rust
pub mod candles;
pub mod pool;
pub mod summary;
pub mod trades;
```

- [ ] **Step 7: Verify compilation**

Run: `cargo build -p auto-trader-db`
Expected: compiles. (DB tests require a running PostgreSQL, tested later in integration.)

- [ ] **Step 8: Commit**

```bash
git add migrations/ crates/db/ && git commit -m "feat(db): add PostgreSQL migrations, trades/candles/summary CRUD"
```

---

### Task 6: OANDA Client and Market Monitor

**Files:**
- Create: `crates/market/src/oanda.rs`
- Create: `crates/market/src/monitor.rs`
- Create: `crates/market/src/indicators.rs`
- Create: `crates/market/src/lib.rs`

- [ ] **Step 1: Write OANDA API client**

`crates/market/src/oanda.rs`:
```rust
use auto_trader_core::types::{Candle, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;

pub struct OandaClient {
    client: reqwest::Client,
    base_url: String,
    account_id: String,
}

#[derive(Debug, Deserialize)]
struct CandlesResponse {
    candles: Vec<OandaCandle>,
}

#[derive(Debug, Deserialize)]
struct OandaCandle {
    time: String,
    volume: Option<i32>,
    mid: OandaCandleMid,
    complete: bool,
}

#[derive(Debug, Deserialize)]
struct OandaCandleMid {
    o: String,
    h: String,
    l: String,
    c: String,
}

impl OandaClient {
    pub fn new(base_url: &str, account_id: &str, api_key: &str) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")).unwrap(),
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        Self {
            client,
            base_url: base_url.to_string(),
            account_id: account_id.to_string(),
        }
    }

    pub async fn get_candles(
        &self,
        pair: &Pair,
        granularity: &str,
        count: u32,
    ) -> anyhow::Result<Vec<Candle>> {
        let url = format!(
            "{}/v3/accounts/{}/instruments/{}/candles",
            self.base_url, self.account_id, pair.0
        );
        let resp: CandlesResponse = self
            .client
            .get(&url)
            .query(&[
                ("granularity", granularity),
                ("count", &count.to_string()),
                ("price", "M"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut candles = Vec::new();
        for c in resp.candles {
            if !c.complete {
                continue;
            }
            let timestamp = DateTime::parse_from_rfc3339(&c.time)?.with_timezone(&Utc);
            candles.push(Candle {
                pair: pair.clone(),
                timeframe: granularity.to_string(),
                open: Decimal::from_str(&c.mid.o)?,
                high: Decimal::from_str(&c.mid.h)?,
                low: Decimal::from_str(&c.mid.l)?,
                close: Decimal::from_str(&c.mid.c)?,
                volume: c.volume,
                timestamp,
            });
        }
        Ok(candles)
    }

    pub async fn get_latest_price(&self, pair: &Pair) -> anyhow::Result<Decimal> {
        let url = format!(
            "{}/v3/accounts/{}/pricing",
            self.base_url, self.account_id
        );
        let resp: serde_json::Value = self
            .client
            .get(&url)
            .query(&[("instruments", pair.0.as_str())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let prices = resp["prices"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("no prices in response"))?;
        let mid = &prices[0];
        let bid = Decimal::from_str(mid["bids"][0]["price"].as_str().unwrap_or("0"))?;
        let ask = Decimal::from_str(mid["asks"][0]["price"].as_str().unwrap_or("0"))?;
        Ok((bid + ask) / Decimal::from(2))
    }
}
```

- [ ] **Step 2: Write technical indicators**

`crates/market/src/indicators.rs`:
```rust
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

pub fn sma(prices: &[Decimal], period: usize) -> Option<Decimal> {
    if prices.len() < period {
        return None;
    }
    let sum: Decimal = prices[prices.len() - period..].iter().sum();
    Some(sum / Decimal::from(period as u64))
}

pub fn ema(prices: &[Decimal], period: usize) -> Option<Decimal> {
    if prices.len() < period {
        return None;
    }
    let multiplier = Decimal::from(2) / Decimal::from((period + 1) as u64);
    let mut ema_val = sma(&prices[..period], period)?;
    for price in &prices[period..] {
        ema_val = (*price - ema_val) * multiplier + ema_val;
    }
    Some(ema_val)
}

pub fn rsi(prices: &[Decimal], period: usize) -> Option<Decimal> {
    if prices.len() < period + 1 {
        return None;
    }
    let changes: Vec<Decimal> = prices.windows(2).map(|w| w[1] - w[0]).collect();
    let recent = &changes[changes.len() - period..];
    let mut avg_gain = Decimal::ZERO;
    let mut avg_loss = Decimal::ZERO;
    for change in recent {
        if *change > Decimal::ZERO {
            avg_gain += change;
        } else {
            avg_loss += change.abs();
        }
    }
    avg_gain /= Decimal::from(period as u64);
    avg_loss /= Decimal::from(period as u64);
    if avg_loss == Decimal::ZERO {
        return Some(Decimal::from(100));
    }
    let rs = avg_gain / avg_loss;
    Some(Decimal::from(100) - Decimal::from(100) / (Decimal::ONE + rs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn sma_basic() {
        let prices = vec![dec!(1), dec!(2), dec!(3), dec!(4), dec!(5)];
        assert_eq!(sma(&prices, 3), Some(dec!(4))); // (3+4+5)/3
    }

    #[test]
    fn sma_insufficient_data() {
        let prices = vec![dec!(1), dec!(2)];
        assert_eq!(sma(&prices, 3), None);
    }

    #[test]
    fn rsi_all_gains() {
        let prices: Vec<Decimal> = (0..=14).map(|i| Decimal::from(i)).collect();
        let result = rsi(&prices, 14).unwrap();
        assert_eq!(result, dec!(100));
    }

    #[test]
    fn rsi_mixed() {
        let prices = vec![
            dec!(44), dec!(44.34), dec!(44.09), dec!(43.61), dec!(44.33),
            dec!(44.83), dec!(45.10), dec!(45.42), dec!(45.84), dec!(46.08),
            dec!(45.89), dec!(46.03), dec!(45.61), dec!(46.28), dec!(46.28),
        ];
        let result = rsi(&prices, 14).unwrap();
        let f = result.to_f64().unwrap();
        assert!(f > 60.0 && f < 80.0, "RSI should be ~70, got {f}");
    }
}
```

- [ ] **Step 3: Run indicator tests**

Run: `cargo test -p auto-trader-market indicators`
Expected: 4 tests pass.

- [ ] **Step 4: Write MarketMonitor**

`crates/market/src/monitor.rs`:
```rust
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::Pair;
use crate::indicators;
use crate::oanda::OandaClient;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

pub struct MarketMonitor {
    client: OandaClient,
    pairs: Vec<Pair>,
    interval_secs: u64,
    tx: mpsc::Sender<PriceEvent>,
}

impl MarketMonitor {
    pub fn new(
        client: OandaClient,
        pairs: Vec<Pair>,
        interval_secs: u64,
        tx: mpsc::Sender<PriceEvent>,
    ) -> Self {
        Self { client, pairs, interval_secs, tx }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let mut tick = interval(Duration::from_secs(self.interval_secs));
        loop {
            tick.tick().await;
            for pair in &self.pairs {
                match self.fetch_and_emit(pair).await {
                    Ok(()) => {}
                    Err(e) => tracing::error!("monitor error for {pair}: {e}"),
                }
            }
        }
    }

    async fn fetch_and_emit(&self, pair: &Pair) -> anyhow::Result<()> {
        let candles = self.client.get_candles(pair, "M5", 100).await?;
        if candles.is_empty() {
            return Ok(());
        }
        let latest = candles.last().unwrap().clone();
        let closes: Vec<Decimal> = candles.iter().map(|c| c.close).collect();

        let mut indicators = HashMap::new();
        if let Some(v) = indicators::sma(&closes, 20) {
            indicators.insert("sma_20".to_string(), v);
        }
        if let Some(v) = indicators::sma(&closes, 50) {
            indicators.insert("sma_50".to_string(), v);
        }
        if let Some(v) = indicators::ema(&closes, 20) {
            indicators.insert("ema_20".to_string(), v);
        }
        if let Some(v) = indicators::rsi(&closes, 14) {
            indicators.insert("rsi_14".to_string(), v);
        }

        let event = PriceEvent {
            pair: pair.clone(),
            candle: latest,
            indicators,
            timestamp: chrono::Utc::now(),
        };
        self.tx.send(event).await?;
        Ok(())
    }
}
```

- [ ] **Step 5: Write lib.rs**

`crates/market/src/lib.rs`:
```rust
pub mod indicators;
pub mod monitor;
pub mod oanda;
```

- [ ] **Step 6: Add dev-dependencies for indicator tests**

Add to `crates/market/Cargo.toml`:
```toml
[dev-dependencies]
rust_decimal_macros = "1"
```

- [ ] **Step 7: Verify compilation**

Run: `cargo build -p auto-trader-market && cargo test -p auto-trader-market`
Expected: compiles, 4 indicator tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/market/ && git commit -m "feat(market): add OANDA client, technical indicators (SMA/EMA/RSI), market monitor"
```

---

### Task 7: Paper Trader

**Files:**
- Create: `crates/executor/src/paper.rs`
- Create: `crates/executor/src/lib.rs`

- [ ] **Step 1: Write PaperTrader with tests**

`crates/executor/src/paper.rs`:
```rust
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::*;
use chrono::Utc;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::Mutex;
use uuid::Uuid;

pub struct PaperTrader {
    balance: Mutex<Decimal>,
    positions: Mutex<HashMap<Uuid, Trade>>,
    leverage: Decimal,
}

impl PaperTrader {
    pub fn new(initial_balance: Decimal, leverage: Decimal) -> Self {
        Self {
            balance: Mutex::new(initial_balance),
            positions: Mutex::new(HashMap::new()),
            leverage,
        }
    }

    pub async fn balance(&self) -> Decimal {
        *self.balance.lock().await
    }

    fn calculate_pnl(direction: Direction, entry: Decimal, exit: Decimal) -> Decimal {
        match direction {
            Direction::Long => exit - entry,
            Direction::Short => entry - exit,
        }
    }
}

impl OrderExecutor for PaperTrader {
    async fn execute(&self, signal: &Signal) -> anyhow::Result<Trade> {
        let trade = Trade {
            id: Uuid::new_v4(),
            strategy_name: signal.strategy_name.clone(),
            pair: signal.pair.clone(),
            direction: signal.direction,
            entry_price: signal.entry_price,
            exit_price: None,
            stop_loss: signal.stop_loss,
            take_profit: signal.take_profit,
            entry_at: Utc::now(),
            exit_at: None,
            pnl_pips: None,
            pnl_amount: None,
            exit_reason: None,
            mode: TradeMode::Paper,
            status: TradeStatus::Open,
        };
        self.positions.lock().await.insert(trade.id, trade.clone());
        tracing::info!(
            "Paper OPEN: {} {} {} @ {}",
            trade.strategy_name,
            trade.pair,
            serde_json::to_string(&trade.direction).unwrap_or_default(),
            trade.entry_price
        );
        Ok(trade)
    }

    async fn open_positions(&self) -> anyhow::Result<Vec<Position>> {
        let positions = self.positions.lock().await;
        Ok(positions
            .values()
            .map(|t| Position { trade: t.clone() })
            .collect())
    }

    async fn close_position(
        &self,
        id: &str,
        exit_reason: ExitReason,
        exit_price: Decimal,
    ) -> anyhow::Result<Trade> {
        let uuid = Uuid::parse_str(id)?;
        let mut positions = self.positions.lock().await;
        let mut trade = positions
            .remove(&uuid)
            .ok_or_else(|| anyhow::anyhow!("position {id} not found"))?;

        let pnl_pips = Self::calculate_pnl(trade.direction, trade.entry_price, exit_price);
        trade.exit_price = Some(exit_price);
        trade.exit_at = Some(Utc::now());
        trade.pnl_pips = Some(pnl_pips);
        trade.pnl_amount = Some(pnl_pips * self.leverage);
        trade.exit_reason = Some(exit_reason);
        trade.status = TradeStatus::Closed;

        let mut balance = self.balance.lock().await;
        *balance += trade.pnl_amount.unwrap_or_default();

        tracing::info!(
            "Paper CLOSE: {} {} pnl={} reason={:?}",
            trade.strategy_name,
            trade.pair,
            pnl_pips,
            exit_reason
        );
        Ok(trade)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_signal() -> Signal {
        Signal {
            strategy_name: "test_strat".to_string(),
            pair: Pair::new("USD_JPY"),
            direction: Direction::Long,
            entry_price: dec!(150.00),
            stop_loss: dec!(149.50),
            take_profit: dec!(151.00),
            confidence: 0.8,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn open_and_close_position() {
        let trader = PaperTrader::new(dec!(100000), dec!(25));
        let trade = trader.execute(&test_signal()).await.unwrap();
        assert_eq!(trade.status, TradeStatus::Open);
        assert_eq!(trade.mode, TradeMode::Paper);

        let positions = trader.open_positions().await.unwrap();
        assert_eq!(positions.len(), 1);

        let closed = trader
            .close_position(&trade.id.to_string(), ExitReason::TpHit, dec!(151.00))
            .await
            .unwrap();
        assert_eq!(closed.status, TradeStatus::Closed);
        assert_eq!(closed.pnl_pips, Some(dec!(1.00)));
        assert_eq!(closed.pnl_amount, Some(dec!(25.00)));

        let positions = trader.open_positions().await.unwrap();
        assert_eq!(positions.len(), 0);

        assert_eq!(trader.balance().await, dec!(100025));
    }

    #[tokio::test]
    async fn short_position_pnl() {
        let trader = PaperTrader::new(dec!(100000), dec!(25));
        let mut signal = test_signal();
        signal.direction = Direction::Short;
        let trade = trader.execute(&signal).await.unwrap();

        let closed = trader
            .close_position(&trade.id.to_string(), ExitReason::SlHit, dec!(150.50))
            .await
            .unwrap();
        assert_eq!(closed.pnl_pips, Some(dec!(-0.50)));
    }

    #[test]
    fn calculate_pnl_long() {
        assert_eq!(
            PaperTrader::calculate_pnl(Direction::Long, dec!(150), dec!(151)),
            dec!(1)
        );
    }

    #[test]
    fn calculate_pnl_short() {
        assert_eq!(
            PaperTrader::calculate_pnl(Direction::Short, dec!(150), dec!(149)),
            dec!(1)
        );
    }
}
```

- [ ] **Step 2: Write lib.rs**

`crates/executor/src/lib.rs`:
```rust
pub mod paper;
```

- [ ] **Step 3: Add dev-dependencies**

Add to `crates/executor/Cargo.toml`:
```toml
[dev-dependencies]
rust_decimal_macros = "1"
serde_json = { workspace = true }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p auto-trader-executor`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/executor/ && git commit -m "feat(executor): add PaperTrader with virtual balance and position management"
```

---

### Task 8: Strategy Engine and TrendFollow V1

**Files:**
- Create: `crates/strategy/src/engine.rs`
- Create: `crates/strategy/src/trend_follow.rs`
- Create: `crates/strategy/src/lib.rs`

- [ ] **Step 1: Write TrendFollowV1 strategy with tests**

`crates/strategy/src/trend_follow.rs`:
```rust
use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{MacroUpdate, Strategy};
use auto_trader_core::types::{Direction, Pair, Signal};
use rust_decimal::Decimal;

pub struct TrendFollowV1 {
    name: String,
    ma_short_period: usize,
    ma_long_period: usize,
    rsi_threshold: Decimal,
    pairs: Vec<Pair>,
    price_history: std::collections::HashMap<String, Vec<Decimal>>,
}

impl TrendFollowV1 {
    pub fn new(
        name: String,
        ma_short: usize,
        ma_long: usize,
        rsi_threshold: Decimal,
        pairs: Vec<Pair>,
    ) -> Self {
        Self {
            name,
            ma_short_period: ma_short,
            ma_long_period: ma_long,
            rsi_threshold,
            pairs,
            price_history: std::collections::HashMap::new(),
        }
    }
}

impl Strategy for TrendFollowV1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }

        let key = event.pair.0.clone();
        let history = self.price_history.entry(key).or_default();
        history.push(event.candle.close);

        if history.len() < self.ma_long_period + 1 {
            return None;
        }

        let sma_short = event.indicators.get("sma_20")?;
        let sma_long = event.indicators.get("sma_50")?;
        let rsi = event.indicators.get("rsi_14")?;

        let prev_closes = &history[..history.len() - 1];
        let prev_sma_short = auto_trader_market::indicators::sma(prev_closes, self.ma_short_period)?;
        let prev_sma_long = auto_trader_market::indicators::sma(prev_closes, self.ma_long_period)?;

        let golden_cross = prev_sma_short <= prev_sma_long && sma_short > sma_long;
        let death_cross = prev_sma_short >= prev_sma_long && sma_short < sma_long;

        if golden_cross && rsi < &self.rsi_threshold {
            let entry = event.candle.close;
            let pip_size = if entry > Decimal::from(10) {
                Decimal::new(1, 2) // JPY pairs: 0.01
            } else {
                Decimal::new(1, 4) // others: 0.0001
            };
            let sl_pips = pip_size * Decimal::from(50);
            let tp_pips = pip_size * Decimal::from(100);

            Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                entry_price: entry,
                stop_loss: entry - sl_pips,
                take_profit: entry + tp_pips,
                confidence: 0.7,
                timestamp: event.timestamp,
            })
        } else if death_cross && rsi > &(Decimal::from(100) - self.rsi_threshold) {
            let entry = event.candle.close;
            let pip_size = if entry > Decimal::from(10) {
                Decimal::new(1, 2)
            } else {
                Decimal::new(1, 4)
            };
            let sl_pips = pip_size * Decimal::from(50);
            let tp_pips = pip_size * Decimal::from(100);

            Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                entry_price: entry,
                stop_loss: entry + sl_pips,
                take_profit: entry - tp_pips,
                confidence: 0.7,
                timestamp: event.timestamp,
            })
        } else {
            None
        }
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {
        // Short-term rule-based strategy ignores macro updates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::Candle;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    fn make_price_event(pair: &str, close: Decimal, indicators: HashMap<String, Decimal>) -> PriceEvent {
        PriceEvent {
            pair: Pair::new(pair),
            candle: Candle {
                pair: Pair::new(pair),
                timeframe: "M5".to_string(),
                open: close,
                high: close,
                low: close,
                close,
                volume: Some(100),
                timestamp: Utc::now(),
            },
            indicators,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn no_signal_insufficient_data() {
        let mut strat = TrendFollowV1::new(
            "test".to_string(), 20, 50, dec!(70), vec![Pair::new("USD_JPY")],
        );
        let event = make_price_event("USD_JPY", dec!(150), HashMap::new());
        assert!(strat.on_price(&event).await.is_none());
    }

    #[tokio::test]
    async fn ignores_untracked_pair() {
        let mut strat = TrendFollowV1::new(
            "test".to_string(), 20, 50, dec!(70), vec![Pair::new("USD_JPY")],
        );
        let event = make_price_event("EUR_USD", dec!(1.10), HashMap::new());
        assert!(strat.on_price(&event).await.is_none());
    }
}
```

- [ ] **Step 2: Add market dependency to strategy crate**

Update `crates/strategy/Cargo.toml` to add:
```toml
auto-trader-market = { workspace = true }
```

And dev-dependencies:
```toml
[dev-dependencies]
rust_decimal_macros = "1"
chrono = { workspace = true }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p auto-trader-strategy trend_follow`
Expected: 2 tests pass.

- [ ] **Step 4: Write StrategyEngine**

`crates/strategy/src/engine.rs`:
```rust
use auto_trader_core::event::{PriceEvent, SignalEvent};
use auto_trader_core::strategy::Strategy;
use tokio::sync::mpsc;

struct StrategySlot {
    strategy: Box<dyn Strategy>,
    mode: String,
}

pub struct StrategyEngine {
    slots: Vec<StrategySlot>,
    signal_tx: mpsc::Sender<SignalEvent>,
}

impl StrategyEngine {
    pub fn new(signal_tx: mpsc::Sender<SignalEvent>) -> Self {
        Self {
            slots: Vec::new(),
            signal_tx,
        }
    }

    pub fn add_strategy(&mut self, strategy: Box<dyn Strategy>, mode: String) {
        self.slots.push(StrategySlot { strategy, mode });
    }

    /// Dispatch PriceEvent to all enabled strategies.
    /// 1-pair-1-position constraint is enforced at the executor level (main.rs),
    /// not here. The engine simply forwards all signals.
    pub async fn on_price(&mut self, event: &PriceEvent) {
        for slot in &mut self.slots {
            if slot.mode == "disabled" {
                continue;
            }
            if let Some(signal) = slot.strategy.on_price(event).await {
                let _ = self.signal_tx.send(SignalEvent { signal }).await;
            }
        }
    }
}
```

- [ ] **Step 5: Write lib.rs**

`crates/strategy/src/lib.rs`:
```rust
pub mod engine;
pub mod trend_follow;
```

- [ ] **Step 6: Verify compilation**

Run: `cargo build -p auto-trader-strategy && cargo test -p auto-trader-strategy`
Expected: compiles, 2 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/strategy/ && git commit -m "feat(strategy): add StrategyEngine with conflict rules and TrendFollowV1 (MA cross + RSI)"
```

---

### Task 9: App Wiring

**Files:**
- Modify: `crates/app/src/main.rs`

- [ ] **Step 1: Wire all components in main.rs**

`crates/app/src/main.rs`:
```rust
use auto_trader_core::config::AppConfig;
use auto_trader_core::event::{PriceEvent, SignalEvent, TradeEvent, TradeAction};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Pair, TradeMode, ExitReason};
use auto_trader_db::pool::create_pool;
use auto_trader_executor::paper::PaperTrader;
use auto_trader_market::monitor::MarketMonitor;
use auto_trader_market::oanda::OandaClient;
use auto_trader_strategy::engine::StrategyEngine;
use auto_trader_strategy::trend_follow::TrendFollowV1;
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config_path = std::env::var("CONFIG_PATH")
        .unwrap_or_else(|_| "config/default.toml".to_string());
    let config = AppConfig::load(&PathBuf::from(&config_path))?;
    tracing::info!("config loaded from {config_path}");

    // Database
    let pool = create_pool(&config.database.url).await?;
    tracing::info!("database connected");

    // Channels
    let (price_tx, mut price_rx) = mpsc::channel::<PriceEvent>(256);
    let (signal_tx, mut signal_rx) = mpsc::channel::<SignalEvent>(256);
    let (trade_tx, mut trade_rx) = mpsc::channel::<TradeEvent>(256);

    // OANDA client
    let api_key = std::env::var("OANDA_API_KEY")
        .expect("OANDA_API_KEY must be set");
    let account_id = std::env::var("OANDA_ACCOUNT_ID")
        .unwrap_or_else(|_| config.oanda.account_id.clone());
    let oanda = OandaClient::new(&config.oanda.api_url, &account_id, &api_key);

    // Market monitor
    let pairs: Vec<Pair> = config.pairs.active.iter().map(|s| Pair::new(s)).collect();
    let monitor = MarketMonitor::new(oanda, pairs, config.monitor.interval_secs, price_tx);

    // Strategy engine
    let mut engine = StrategyEngine::new(signal_tx);
    for sc in &config.strategies {
        if !sc.enabled {
            continue;
        }
        match sc.name.as_str() {
            name if name.starts_with("trend_follow") => {
                let ma_short = sc.params.get("ma_short")
                    .and_then(|v| v.as_integer()).unwrap_or(20) as usize;
                let ma_long = sc.params.get("ma_long")
                    .and_then(|v| v.as_integer()).unwrap_or(50) as usize;
                let rsi_thresh = sc.params.get("rsi_threshold")
                    .and_then(|v| v.as_integer()).unwrap_or(70);
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                engine.add_strategy(
                    Box::new(TrendFollowV1::new(
                        sc.name.clone(),
                        ma_short,
                        ma_long,
                        Decimal::from(rsi_thresh),
                        pairs,
                    )),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            other => {
                tracing::warn!("unknown strategy: {other}, skipping");
            }
        }
    }

    // Paper trader
    let paper_trader = Arc::new(PaperTrader::new(
        Decimal::from(100_000),
        Decimal::from(25),
    ));

    // Task: Market monitor
    let monitor_handle = tokio::spawn(async move {
        if let Err(e) = monitor.run().await {
            tracing::error!("monitor error: {e}");
        }
    });

    // Task: Strategy engine (price -> signal)
    let engine_handle = tokio::spawn(async move {
        while let Some(event) = price_rx.recv().await {
            engine.on_price(&event).await;
        }
    });

    // Task: Signal executor (signal -> trade)
    // Enforces 1-pair-1-position per strategy at execution time
    let executor = paper_trader.clone();
    let trade_tx_clone = trade_tx.clone();
    let executor_handle = tokio::spawn(async move {
        while let Some(signal_event) = signal_rx.recv().await {
            let signal = &signal_event.signal;
            // Check 1-pair-1-position constraint per strategy
            let positions = executor.open_positions().await.unwrap_or_default();
            let has_position = positions.iter().any(|p| {
                p.trade.strategy_name == signal.strategy_name && p.trade.pair == signal.pair
            });
            if has_position {
                tracing::debug!(
                    "skipping signal: {} already has open position for {}",
                    signal.strategy_name, signal.pair
                );
                continue;
            }
            match executor.execute(signal).await {
                Ok(trade) => {
                    let _ = trade_tx_clone.send(TradeEvent {
                        trade,
                        action: TradeAction::Opened,
                    }).await;
                }
                Err(e) => tracing::error!("execute error: {e}"),
            }
        }
    });

    // Task: Trade recorder (trade -> DB)
    let recorder_pool = pool.clone();
    let recorder_handle = tokio::spawn(async move {
        while let Some(trade_event) = trade_rx.recv().await {
            match trade_event.action {
                TradeAction::Opened => {
                    if let Err(e) = auto_trader_db::trades::insert_trade(
                        &recorder_pool,
                        &trade_event.trade,
                    ).await {
                        tracing::error!("record trade error: {e}");
                    }
                }
                TradeAction::Closed { .. } => {
                    let t = &trade_event.trade;
                    if let (Some(exit_price), Some(exit_at), Some(pnl_pips), Some(pnl_amount), Some(exit_reason)) =
                        (t.exit_price, t.exit_at, t.pnl_pips, t.pnl_amount, t.exit_reason)
                    {
                        if let Err(e) = auto_trader_db::trades::update_trade_closed(
                            &recorder_pool, t.id, exit_price, exit_at, pnl_pips, pnl_amount, exit_reason,
                        ).await {
                            tracing::error!("update trade error: {e}");
                        }
                        // Upsert daily summary
                        let date = exit_at.date_naive();
                        let mode_str = serde_json::to_string(&t.mode).unwrap_or_default();
                        let mode_str = mode_str.trim_matches('"');
                        let win = if pnl_pips > Decimal::ZERO { 1 } else { 0 };
                        let _ = auto_trader_db::summary::upsert_daily_summary(
                            &recorder_pool, date, &t.strategy_name, &t.pair.0,
                            mode_str, 1, win, pnl_amount,
                        ).await;
                    }
                }
            }
        }
    });

    tracing::info!("auto-trader running. Press Ctrl+C to stop.");

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down...");

    monitor_handle.abort();
    engine_handle.abort();
    executor_handle.abort();
    recorder_handle.abort();

    Ok(())
}
```

- [ ] **Step 2: Add missing dependencies to app crate**

Update `crates/app/Cargo.toml`:
```toml
[dependencies]
auto-trader-core = { workspace = true }
auto-trader-db = { workspace = true }
auto-trader-market = { workspace = true }
auto-trader-strategy = { workspace = true }
auto-trader-executor = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow = { workspace = true }
rust_decimal = { workspace = true }
serde_json = { workspace = true }
```

- [ ] **Step 3: Verify compilation**

Run: `cargo build`
Expected: full workspace compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/app/ && git commit -m "feat(app): wire market-monitor -> strategy-engine -> executor -> recorder pipeline"
```

---

### Task 10: Docker Setup

**Files:**
- Create: `Dockerfile`
- Create: `docker-compose.yml`
- Create: `.env.example`

- [ ] **Step 1: Create Dockerfile**

```dockerfile
FROM rust:1.85-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY migrations/ migrations/
RUN cargo build --release --bin auto-trader

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/auto-trader /usr/local/bin/auto-trader
COPY config/ /app/config/
COPY migrations/ /app/migrations/
WORKDIR /app
ENV CONFIG_PATH=/app/config/default.toml
CMD ["auto-trader"]
```

- [ ] **Step 2: Create docker-compose.yml**

```yaml
services:
  db:
    image: postgres:16-alpine
    environment:
      POSTGRES_USER: auto-trader
      POSTGRES_PASSWORD: auto-trader
      POSTGRES_DB: auto_trader
    ports:
      - "5432:5432"
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U auto-trader"]
      interval: 5s
      timeout: 5s
      retries: 5

  auto-trader:
    build: .
    depends_on:
      db:
        condition: service_healthy
    environment:
      CONFIG_PATH: /app/config/default.toml
      OANDA_API_KEY: ${OANDA_API_KEY}
      OANDA_ACCOUNT_ID: ${OANDA_ACCOUNT_ID}
      RUST_LOG: info
    volumes:
      - ./config:/app/config:ro

volumes:
  pgdata:
```

- [ ] **Step 3: Create .env.example**

```
OANDA_API_KEY=your-oanda-api-key-here
OANDA_ACCOUNT_ID=your-account-id-here
```

- [ ] **Step 4: Create .gitignore**

```
/target
.env
*.swp
*.swo
```

- [ ] **Step 5: Verify docker-compose builds**

Run: `docker compose build`
Expected: builds successfully.

- [ ] **Step 6: Commit**

```bash
git add Dockerfile docker-compose.yml .env.example .gitignore && git commit -m "feat: add Docker and docker-compose configuration"
```

---

### Task 11: Integration Test

**Files:**
- Create: `tests/integration_test.rs` (workspace level)

- [ ] **Step 1: Write integration test**

This test requires a running PostgreSQL. Use docker-compose to start just the DB:

Run: `docker compose up -d db`

Create `tests/integration_test.rs`:
```rust
//! Integration test: requires PostgreSQL running on localhost:5432
//! Run: docker compose up -d db
//! Then: cargo test --test integration_test

use auto_trader_core::event::{PriceEvent, SignalEvent};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::*;
use auto_trader_executor::paper::PaperTrader;
use auto_trader_market::indicators;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tokio::sync::mpsc;

#[tokio::test]
async fn paper_trade_roundtrip() {
    let trader = PaperTrader::new(dec!(100000), dec!(25));

    let signal = Signal {
        strategy_name: "test".to_string(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        entry_price: dec!(150.00),
        stop_loss: dec!(149.50),
        take_profit: dec!(151.00),
        confidence: 0.8,
        timestamp: Utc::now(),
    };

    // Open
    let trade = trader.execute(&signal).await.unwrap();
    assert_eq!(trade.status, TradeStatus::Open);

    // Close with profit
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit, dec!(151.00))
        .await
        .unwrap();
    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(closed.pnl_pips.unwrap(), dec!(1.00));

    // Balance updated
    assert_eq!(trader.balance().await, dec!(100025));
}

#[tokio::test]
async fn indicators_consistency() {
    let prices: Vec<Decimal> = (0..100).map(|i| dec!(100) + Decimal::from(i) / dec!(10)).collect();
    let sma20 = indicators::sma(&prices, 20).unwrap();
    let sma50 = indicators::sma(&prices, 50).unwrap();
    // In an uptrend, short MA > long MA
    assert!(sma20 > sma50, "sma20={sma20} should be > sma50={sma50}");
}

#[tokio::test]
async fn channel_pipeline() {
    let (signal_tx, mut signal_rx) = mpsc::channel::<SignalEvent>(16);

    let signal = Signal {
        strategy_name: "test".to_string(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        entry_price: dec!(150.00),
        stop_loss: dec!(149.50),
        take_profit: dec!(151.00),
        confidence: 0.8,
        timestamp: Utc::now(),
    };

    signal_tx.send(SignalEvent { signal: signal.clone() }).await.unwrap();
    let received = signal_rx.recv().await.unwrap();
    assert_eq!(received.signal.pair, Pair::new("USD_JPY"));
    assert_eq!(received.signal.direction, Direction::Long);
}
```

- [ ] **Step 2: Add workspace-level test dependencies**

Add to root `Cargo.toml`:
```toml
[workspace.dependencies]
rust_decimal_macros = "1"
```

Create `tests/` directory and ensure Cargo finds it (it does by default for workspace root).

Actually, integration tests at the workspace root require a package. Better to put this in `crates/app/tests/integration_test.rs` or use a dedicated test crate. For simplicity, add to `crates/app/`:

Move file to `crates/app/tests/integration_test.rs`.

Add to `crates/app/Cargo.toml`:
```toml
[dev-dependencies]
rust_decimal_macros = "1"
chrono = { workspace = true }
uuid = { workspace = true }
```

- [ ] **Step 3: Run integration tests**

Run: `cargo test -p auto-trader --test integration_test`
Expected: 3 tests pass.

- [ ] **Step 4: Run full test suite**

Run: `cargo test --workspace`
Expected: all tests across all crates pass.

- [ ] **Step 5: Commit**

```bash
git add crates/app/tests/ crates/app/Cargo.toml Cargo.toml && git commit -m "test: add integration tests for paper trading pipeline"
```

---

### Task 12: Generate Cargo.lock and Final Verification

- [ ] **Step 1: Generate lock file**

Run: `cargo generate-lockfile`

- [ ] **Step 2: Full build and test**

Run: `cargo build --release && cargo test --workspace`
Expected: release build succeeds, all tests pass.

- [ ] **Step 3: Verify docker-compose**

Run: `docker compose up -d db && sleep 3 && docker compose build auto-trader`
Expected: builds. (Don't run auto-trader yet - needs OANDA API key.)

Run: `docker compose down`

- [ ] **Step 4: Final commit**

```bash
git add Cargo.lock && git commit -m "chore: add Cargo.lock"
```
