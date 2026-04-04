# Phase 0 Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phase 0 の全スコープを実装し、TrendFollowV1 + SwingLLMv1 を 24/365 でペーパートレード運用できる状態にする。Vegapunk 連携・マクロ分析・バックテストを含む。

**Architecture:** 既存の tokio mpsc channel パイプラインに position-monitor（SL/TP 監視）、vegapunk-client（gRPC）、macro-analyst（経済指標+ニュース→Vegapunk）、backtest（過去データリプレイ）を追加する。swing_llm_v1 は on_price 内で Vegapunk Search + Gemini Flash API を呼び出す。

**Tech Stack:** 既存スタック + tonic (gRPC), tonic-build (proto codegen), reqwest (Gemini API, News API), feed-rs (RSS), google-generative-ai or raw HTTP (Gemini Flash)

**Plan:** 2 of 3 (Trading Core [done] -> Phase 0 Completion [this] -> Dashboard)

**前提:**
- ビルド・テストは Docker 経由: `docker run --rm -v "$(pwd):/app" -w /app rust:1.85-bookworm cargo test --workspace`
- ローカルに Rust toolchain なし
- Vegapunk は `fuj11-agent-01:3000` で稼働中（または稼働予定）
- API キーは 1Password + direnv（`op read` パターン）

**参照ドキュメント:**
- `specs/design.md` — メイン設計書（実装状況テーブル付き）
- `specs/vegapunk-integration.md` — Vegapunk 連携仕様・スキーマ設計
- Vegapunk proto: `https://github.com/ryugou/vegapunk/blob/main/proto/graphrag.proto`

---

## Batch Structure

```
Batch 1 (並行可能): Task 1-4
  Task 1: Stability fixes (unwrap, memory leak, graceful shutdown, OANDA retry)
  Task 2: SL/TP position-monitor タスク
  Task 3: Candle DB 保存
  Task 4: vegapunk-client crate (tonic gRPC)

Batch 2 (Task 4 完了後): Task 5-6
  Task 5: macro-analyst crate (経済指標 + ニュース → Vegapunk)
  Task 6: swing_llm_v1 戦略 (Vegapunk Search + Gemini Flash)

Batch 3 (Task 3 完了後): Task 7
  Task 7: backtest crate (過去データリプレイ)

Batch 4 (Batch 1 完了後): Task 8-9
  Task 8: max_drawdown 日次バッチ
  Task 9: Vegapunk ingestion in recorder (トレード判断・決済の蓄積)

Batch 5: Task 10
  Task 10: Pipeline 結合・config 更新・docker-compose 更新・E2E 検証
```

---

## File Structure (新規・変更のみ)

```
auto-trader/
  Cargo.toml                              # MODIFY: add vegapunk-client, macro-analyst, backtest members
  proto/
    graphrag.proto                        # CREATE: copy from vegapunk repo
  schemas/
    fx-trading.yml                        # CREATE: Vegapunk schema definition
  crates/
    core/src/
      types.rs                            # MODIFY: add MacroEvent type
      config.rs                           # MODIFY: add MacroAnalystConfig, GeminiConfig
    market/src/
      oanda.rs                            # MODIFY: add timeout, retry
      monitor.rs                          # MODIFY: candle DB save, unwrap fix, channel close
    strategy/src/
      lib.rs                              # MODIFY: add swing_llm module
      trend_follow.rs                     # MODIFY: VecDeque rolling window
      swing_llm.rs                        # CREATE: SwingLLMv1 strategy
    executor/src/
      paper.rs                            # MODIFY: (no changes needed, trait already correct)
    app/src/
      main.rs                             # MODIFY: add position-monitor, graceful shutdown, new tasks
    vegapunk-client/
      Cargo.toml                          # CREATE
      build.rs                            # CREATE: tonic-build for proto
      src/
        lib.rs                            # CREATE: re-exports
        client.rs                         # CREATE: VegapunkClient wrapper
    macro-analyst/
      Cargo.toml                          # CREATE
      src/
        lib.rs                            # CREATE: re-exports
        analyst.rs                        # CREATE: MacroAnalyst orchestrator
        calendar.rs                       # CREATE: economic calendar fetcher
        news.rs                           # CREATE: news fetcher (RSS/API)
        summarizer.rs                     # CREATE: Gemini Flash summarizer
    backtest/
      Cargo.toml                          # CREATE
      src/
        lib.rs                            # CREATE: re-exports
        runner.rs                         # CREATE: BacktestRunner
        report.rs                         # CREATE: BacktestReport
    db/src/
      summary.rs                          # MODIFY: add max_drawdown calculation
      macro_events.rs                     # CREATE: macro_events CRUD
  config/
    default.toml                          # MODIFY: add macro_analyst, gemini, swing_llm_v1 config
  docker-compose.yml                      # MODIFY: add env vars for new API keys
```

---

### Task 1: Stability Fixes

**Files:**
- Modify: `crates/market/src/oanda.rs`
- Modify: `crates/market/src/monitor.rs`
- Modify: `crates/strategy/src/trend_follow.rs`
- Modify: `crates/app/src/main.rs`

**Goal:** 24/365 運用でクラッシュしない安定性を確保する。

- [ ] **Step 1: OandaClient にタイムアウトとリトライを追加**

`crates/market/src/oanda.rs` の `new()` を修正:

```rust
pub fn new(base_url: &str, account_id: &str, api_key: &str) -> anyhow::Result<Self> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "Authorization",
        reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))?,
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    Ok(Self {
        client,
        base_url: base_url.to_string(),
        account_id: account_id.to_string(),
    })
}
```

`get_candles` と `get_latest_price` に最大 3 回のリトライを追加:

```rust
async fn request_with_retry<T, F, Fut>(&self, f: F) -> anyhow::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut last_err = None;
    for attempt in 0..3 {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                tracing::warn!("OANDA request failed (attempt {}): {e}", attempt + 1);
                last_err = Some(e);
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt as u32))).await;
                }
            }
        }
    }
    Err(last_err.unwrap())
}
```

- [ ] **Step 2: monitor.rs の unwrap 除去**

`crates/market/src/monitor.rs` の `fetch_and_emit` を修正:

```rust
let latest = match candles.last() {
    Some(c) => c.clone(),
    None => return Ok(()),  // empty after filtering incomplete candles
};
```

- [ ] **Step 3: TrendFollowV1 の price_history を VecDeque + rolling window に変更**

`crates/strategy/src/trend_follow.rs`:

```rust
use std::collections::{HashMap, VecDeque};

pub struct TrendFollowV1 {
    // ...existing fields...
    price_history: HashMap<String, VecDeque<Decimal>>,
}
```

`on_price` 内で push 後にトリミング:

```rust
let key = event.pair.0.clone();
let history = self.price_history.entry(key).or_default();
history.push_back(event.candle.close);

// Keep only what we need: ma_long_period + 1 for cross detection
let max_len = self.ma_long_period + 2;
while history.len() > max_len {
    history.pop_front();
}
```

`indicators::sma` が `&[Decimal]` を受け取るので、VecDeque を Vec に変換する必要がある:

```rust
let closes: Vec<Decimal> = history.iter().copied().collect();
if closes.len() < self.ma_long_period + 1 {
    return None;
}
let sma_short = auto_trader_market::indicators::sma(&closes, self.ma_short_period)?;
```

- [ ] **Step 4: Graceful shutdown を実装**

`crates/app/src/main.rs` の shutdown 部分を変更:

```rust
tracing::info!("auto-trader running. Press Ctrl+C to stop.");

tokio::signal::ctrl_c().await?;
tracing::info!("shutting down... draining channels");

// Drop senders to signal tasks to finish
drop(price_tx);  // monitor は別途 abort が必要（無限ループのため）
monitor_handle.abort();

// Wait for downstream tasks to drain (max 5 seconds)
let drain_timeout = tokio::time::Duration::from_secs(5);
let _ = tokio::time::timeout(drain_timeout, async {
    let _ = engine_handle.await;
    let _ = executor_handle.await;
    let _ = recorder_handle.await;
}).await;

tracing::info!("shutdown complete");
Ok(())
```

Note: `price_tx` を drop できるよう、monitor タスクに渡す前に clone する設計変更が必要。monitor は自身の `tx` を持っているので、main で保持している `price_tx` は不要な場合は別途設計を確認すること。

- [ ] **Step 5: テスト実行**

Run: `docker run --rm -v "$(pwd):/app" -w /app rust:1.85-bookworm cargo test --workspace`
Expected: 全テスト PASS、警告 0 件

- [ ] **Step 6: Commit**

```bash
git add crates/market/src/oanda.rs crates/market/src/monitor.rs crates/strategy/src/trend_follow.rs crates/app/src/main.rs
git commit -m "fix: stability improvements for 24/7 operation

- Add timeout (30s) and retry (3x exponential backoff) to OANDA client
- Remove unwrap on empty candle response in monitor
- Use VecDeque rolling window in TrendFollowV1 to prevent memory leak
- Implement graceful shutdown with channel drain timeout"
```

---

### Task 2: SL/TP Position Monitor

**Files:**
- Create: `crates/app/src/position_monitor.rs`
- Modify: `crates/app/src/main.rs`
- Modify: `crates/app/Cargo.toml` (if needed)
- Test: `crates/app/tests/integration_test.rs`

**Goal:** PriceEvent を購読し、オープンポジションの SL/TP をチェック。ヒット時に自動決済して TradeEvent を emit する。

- [ ] **Step 1: position_monitor モジュールを作成**

`crates/app/src/position_monitor.rs`:

```rust
use auto_trader_core::event::{PriceEvent, TradeEvent, TradeAction};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, ExitReason};
use std::sync::Arc;
use tokio::sync::mpsc;

pub async fn run_position_monitor<E: OrderExecutor>(
    executor: Arc<E>,
    mut price_rx: mpsc::Receiver<PriceEvent>,
    trade_tx: mpsc::Sender<TradeEvent>,
) {
    while let Some(event) = price_rx.recv().await {
        let current_price = event.candle.close;
        let positions = match executor.open_positions().await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("position monitor: failed to get positions: {e}");
                continue;
            }
        };

        for pos in positions {
            let trade = &pos.trade;
            if trade.pair != event.pair {
                continue;
            }

            let exit_reason = match trade.direction {
                Direction::Long => {
                    if current_price <= trade.stop_loss {
                        Some(ExitReason::SlHit)
                    } else if current_price >= trade.take_profit {
                        Some(ExitReason::TpHit)
                    } else {
                        None
                    }
                }
                Direction::Short => {
                    if current_price >= trade.stop_loss {
                        Some(ExitReason::SlHit)
                    } else if current_price <= trade.take_profit {
                        Some(ExitReason::TpHit)
                    } else {
                        None
                    }
                }
            };

            if let Some(reason) = exit_reason {
                let exit_price = match reason {
                    ExitReason::SlHit => trade.stop_loss,
                    ExitReason::TpHit => trade.take_profit,
                    _ => current_price,
                };

                match executor.close_position(&trade.id.to_string(), reason, exit_price).await {
                    Ok(closed_trade) => {
                        tracing::info!(
                            "position closed: {} {} {:?} at {} ({:?})",
                            closed_trade.strategy_name, closed_trade.pair,
                            closed_trade.direction, exit_price, reason
                        );
                        let _ = trade_tx.send(TradeEvent {
                            trade: closed_trade,
                            action: TradeAction::Closed { exit_price, exit_reason: reason },
                        }).await;
                    }
                    Err(e) => tracing::error!("failed to close position: {e}"),
                }
            }
        }
    }
    tracing::info!("position monitor: price channel closed, stopping");
}
```

- [ ] **Step 2: main.rs に position-monitor タスクを追加**

`crates/app/src/main.rs` に以下を追加:

1. `mod position_monitor;` を追加
2. price channel を broadcast に変更するか、2 つ目の receiver を作る。最もシンプルなのは price_tx を clone して 2 つの subscriber に送る方式:

```rust
// Channels — price は position_monitor にも配信するため 2 本
let (price_tx, mut price_rx) = mpsc::channel::<PriceEvent>(256);
let (price_monitor_tx, price_monitor_rx) = mpsc::channel::<PriceEvent>(256);
```

monitor の `fetch_and_emit` で PriceEvent を 2 箇所に送る。あるいは main の engine タスク内で forward する:

```rust
// Task: Strategy engine (price -> signal) + forward to position monitor
let engine_handle = tokio::spawn(async move {
    while let Some(event) = price_rx.recv().await {
        // Forward to position monitor
        let _ = price_monitor_tx.send(event.clone()).await;
        engine.on_price(&event).await;
    }
});

// Task: Position monitor (price -> SL/TP check -> close)
let pos_monitor_executor = paper_trader.clone();
let pos_monitor_trade_tx = trade_tx.clone();
let pos_monitor_handle = tokio::spawn(async move {
    position_monitor::run_position_monitor(
        pos_monitor_executor,
        price_monitor_rx,
        pos_monitor_trade_tx,
    ).await;
});
```

- [ ] **Step 3: 統合テストを追加**

`crates/app/tests/integration_test.rs` に追加:

```rust
#[tokio::test]
async fn sl_tp_auto_close() {
    let trader = PaperTrader::new(dec!(100000), dec!(25));

    // Open a long position
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
    let trade = trader.execute(&signal).await.unwrap();
    assert_eq!(trade.status, TradeStatus::Open);

    // SL hit: close at stop_loss price
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit, dec!(149.50))
        .await
        .unwrap();
    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(closed.exit_reason, Some(ExitReason::SlHit));
    assert_eq!(closed.exit_price, Some(dec!(149.50)));
    // PnL: (149.50 - 150.00) = -0.50
    assert_eq!(closed.pnl_pips.unwrap(), dec!(-0.50));
}
```

- [ ] **Step 4: テスト実行**

Run: `docker run --rm -v "$(pwd):/app" -w /app rust:1.85-bookworm cargo test --workspace`
Expected: 全テスト PASS

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/position_monitor.rs crates/app/src/main.rs crates/app/tests/integration_test.rs
git commit -m "feat: add SL/TP position monitor for automatic trade closure

Position monitor subscribes to PriceEvent, checks open positions against
SL/TP levels, and closes via OrderExecutor when hit. Enables 24/7 paper
trading with automatic position lifecycle management."
```

---

### Task 3: Candle DB Persistence

**Files:**
- Modify: `crates/market/src/monitor.rs`
- Modify: `crates/market/Cargo.toml` (add auto-trader-db dependency)

**Goal:** MarketMonitor が取得した candle を price_candles テーブルに保存し、バックテスト用データを蓄積する。

- [ ] **Step 1: MarketMonitor に PgPool を渡す**

`crates/market/src/monitor.rs` を修正:

```rust
use sqlx::PgPool;

pub struct MarketMonitor {
    client: OandaClient,
    pairs: Vec<Pair>,
    interval_secs: u64,
    tx: mpsc::Sender<PriceEvent>,
    pool: Option<PgPool>,  // Option for backward compatibility with tests
}

impl MarketMonitor {
    pub fn new(
        client: OandaClient,
        pairs: Vec<Pair>,
        interval_secs: u64,
        tx: mpsc::Sender<PriceEvent>,
    ) -> Self {
        Self { client, pairs, interval_secs, tx, pool: None }
    }

    pub fn with_db(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }
}
```

- [ ] **Step 2: fetch_and_emit に candle 保存を追加**

```rust
async fn fetch_and_emit(&self, pair: &Pair) -> anyhow::Result<()> {
    let candles = self.client.get_candles(pair, "M5", 100).await?;
    if candles.is_empty() {
        return Ok(());
    }
    let latest = match candles.last() {
        Some(c) => c.clone(),
        None => return Ok(()),
    };

    // Save candles to DB for backtest data accumulation
    if let Some(pool) = &self.pool {
        for candle in &candles {
            if let Err(e) = auto_trader_db::candles::upsert_candle(pool, candle).await {
                tracing::warn!("failed to save candle: {e}");
            }
        }
    }

    // ... rest of indicator calculation and PriceEvent emission (unchanged)
}
```

- [ ] **Step 3: main.rs で with_db を呼ぶ**

```rust
let monitor = MarketMonitor::new(oanda, pairs, config.monitor.interval_secs, price_tx)
    .with_db(pool.clone());
```

- [ ] **Step 4: Cargo.toml に依存追加**

`crates/market/Cargo.toml` に追加:

```toml
auto-trader-db = { workspace = true }
sqlx = { workspace = true }
```

- [ ] **Step 5: テスト実行**

Run: `docker run --rm -v "$(pwd):/app" -w /app rust:1.85-bookworm cargo test --workspace`
Expected: 全テスト PASS

- [ ] **Step 6: Commit**

```bash
git add crates/market/ crates/app/src/main.rs
git commit -m "feat: persist candles to DB for backtest data accumulation

MarketMonitor now saves fetched candles to price_candles table via
upsert_candle(). DB connection is optional (with_db builder) for
backward compatibility with unit tests."
```

---

### Task 4: Vegapunk Client Crate

**Files:**
- Create: `proto/graphrag.proto`
- Create: `crates/vegapunk-client/Cargo.toml`
- Create: `crates/vegapunk-client/build.rs`
- Create: `crates/vegapunk-client/src/lib.rs`
- Create: `crates/vegapunk-client/src/client.rs`
- Modify: `Cargo.toml` (workspace)

**Goal:** Vegapunk の gRPC API（IngestRaw, Search, Feedback, Merge）を呼び出す Rust クライアントを作成する。

- [ ] **Step 1: proto ファイルをコピー**

Vegapunk リポジトリから `proto/graphrag.proto` をコピー:

```bash
mkdir -p proto
gh api repos/ryugou/vegapunk/contents/proto/graphrag.proto --jq '.content' | base64 -d > proto/graphrag.proto
```

- [ ] **Step 2: workspace に crate を追加**

`Cargo.toml` の `[workspace]` members に `"crates/vegapunk-client"` を追加。

`[workspace.dependencies]` に追加:

```toml
tonic = "0.12"
tonic-build = "0.12"
prost = "0.13"
auto-trader-vegapunk = { path = "crates/vegapunk-client" }
```

- [ ] **Step 3: Cargo.toml 作成**

`crates/vegapunk-client/Cargo.toml`:

```toml
[package]
name = "auto-trader-vegapunk"
version = "0.1.0"
edition.workspace = true

[dependencies]
tonic = { workspace = true }
prost = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }

[build-dependencies]
tonic-build = { workspace = true }
```

- [ ] **Step 4: build.rs 作成**

`crates/vegapunk-client/build.rs`:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false)
        .compile_protos(&["../../proto/graphrag.proto"], &["../../proto"])?;
    Ok(())
}
```

- [ ] **Step 5: client.rs 作成**

`crates/vegapunk-client/src/client.rs`:

```rust
use crate::proto::graph_rag_engine_client::GraphRagEngineClient;
use crate::proto::*;
use tonic::transport::Channel;

pub struct VegapunkClient {
    client: GraphRagEngineClient<Channel>,
    schema: String,
}

impl VegapunkClient {
    pub async fn connect(endpoint: &str, schema: &str) -> anyhow::Result<Self> {
        let client = GraphRagEngineClient::connect(endpoint.to_string()).await?;
        Ok(Self {
            client,
            schema: schema.to_string(),
        })
    }

    pub async fn ingest_raw(
        &mut self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<IngestRawResponse> {
        let request = IngestRawRequest {
            text: text.to_string(),
            metadata: Some(IngestRawMetadata {
                source_type: source_type.to_string(),
                author: None,
                channel: Some(channel.to_string()),
                timestamp: Some(timestamp.to_string()),
            }),
            schema: Some(self.schema.clone()),
        };
        let response = self.client.ingest_raw(request).await?;
        Ok(response.into_inner())
    }

    pub async fn search(
        &mut self,
        query: &str,
        mode: &str,
        top_k: i32,
    ) -> anyhow::Result<SearchResponse> {
        let request = SearchRequest {
            text: query.to_string(),
            filter: None,
            depth: None,
            top_k: Some(top_k),
            format: None,
            mode: Some(mode.to_string()),
            schema: Some(self.schema.clone()),
            offset: None,
            limit: None,
            structural_weight: None,
        };
        let response = self.client.search(request).await?;
        Ok(response.into_inner())
    }

    pub async fn feedback(
        &mut self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()> {
        let request = FeedbackRequest {
            search_id: search_id.to_string(),
            rating,
            comment: comment.to_string(),
        };
        self.client.feedback(request).await?;
        Ok(())
    }

    pub async fn merge(&mut self) -> anyhow::Result<()> {
        let request = MergeRequest {
            schema: Some(self.schema.clone()),
        };
        self.client.merge(request).await?;
        Ok(())
    }
}
```

- [ ] **Step 6: lib.rs 作成**

`crates/vegapunk-client/src/lib.rs`:

```rust
pub mod client;

pub mod proto {
    tonic::include_proto!("graphrag");
}
```

- [ ] **Step 7: ビルド確認**

Run: `docker run --rm -v "$(pwd):/app" -w /app rust:1.85-bookworm bash -c "apt-get update && apt-get install -y protobuf-compiler && cargo build -p auto-trader-vegapunk"`

Note: proto コンパイルに `protoc` が必要。Dockerfile にも追加が必要。

Expected: ビルド成功

- [ ] **Step 8: Commit**

```bash
git add proto/ crates/vegapunk-client/ Cargo.toml
git commit -m "feat: add vegapunk-client crate with tonic gRPC client

Implements IngestRaw, Search, Feedback, Merge RPCs against Vegapunk
GraphRAG engine. Proto copied from vegapunk repository."
```

---

### Task 5: Macro Analyst Crate

**Files:**
- Create: `crates/macro-analyst/Cargo.toml`
- Create: `crates/macro-analyst/src/lib.rs`
- Create: `crates/macro-analyst/src/analyst.rs`
- Create: `crates/macro-analyst/src/calendar.rs`
- Create: `crates/macro-analyst/src/news.rs`
- Create: `crates/macro-analyst/src/summarizer.rs`
- Create: `crates/db/src/macro_events.rs`
- Modify: `crates/db/src/lib.rs`
- Modify: `crates/core/src/config.rs`
- Modify: `Cargo.toml` (workspace)

**Goal:** 経済指標カレンダーと FX ニュースを定期取得し、Gemini Flash で要約して Vegapunk に IngestRaw する。MacroUpdate を戦略に配信する。

- [ ] **Step 1: AppConfig に MacroAnalystConfig と GeminiConfig を追加**

`crates/core/src/config.rs`:

```rust
#[derive(Debug, Deserialize)]
pub struct MacroAnalystConfig {
    pub enabled: bool,
    pub calendar_interval_secs: u64,  // e.g. 3600 (hourly)
    pub news_interval_secs: u64,      // e.g. 1800 (30 min)
    pub news_sources: Vec<String>,    // RSS feed URLs
}

#[derive(Debug, Deserialize)]
pub struct GeminiConfig {
    pub model: String,  // e.g. "gemini-2.0-flash"
    pub api_url: String,
    // api_key from env var GEMINI_API_KEY
}
```

`AppConfig` に `macro_analyst: Option<MacroAnalystConfig>` と `gemini: Option<GeminiConfig>` を追加。

- [ ] **Step 2: macro_events DB 関数を作成**

`crates/db/src/macro_events.rs`:

```rust
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn insert_macro_event(
    pool: &PgPool,
    summary: &str,
    event_type: &str,
    impact: &str,
    event_at: DateTime<Utc>,
    source: Option<&str>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO macro_events (id, summary, event_type, impact, event_at, source)
         VALUES ($1, $2, $3, $4, $5, $6)"
    )
    .bind(id)
    .bind(summary)
    .bind(event_type)
    .bind(impact)
    .bind(event_at)
    .bind(source)
    .execute(pool)
    .await?;
    Ok(id)
}
```

`crates/db/src/lib.rs` に `pub mod macro_events;` を追加。

- [ ] **Step 3: Gemini Flash summarizer を作成**

`crates/macro-analyst/src/summarizer.rs`:

```rust
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct GeminiSummarizer {
    client: Client,
    api_url: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
}

#[derive(Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiContentResponse,
}

#[derive(Deserialize)]
struct GeminiContentResponse {
    parts: Vec<GeminiPartResponse>,
}

#[derive(Deserialize)]
struct GeminiPartResponse {
    text: String,
}

impl GeminiSummarizer {
    pub fn new(api_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            client: Client::new(),
            api_url: api_url.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }

    pub async fn summarize_for_fx(&self, text: &str) -> anyhow::Result<String> {
        let prompt = format!(
            "以下のニュース/経済指標をFXトレードの観点で要約してください。\
             影響を受ける通貨ペアと方向性（強気/弱気）を含めてください。\
             日本語で3文以内で。\n\n{text}"
        );

        let request = GeminiRequest {
            contents: vec![GeminiContent {
                parts: vec![GeminiPart { text: prompt }],
            }],
        };

        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            self.api_url, self.model, self.api_key
        );

        let resp: GeminiResponse = self.client
            .post(&url)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let text = resp.candidates
            .first()
            .and_then(|c| c.content.parts.first())
            .map(|p| p.text.clone())
            .unwrap_or_default();

        Ok(text)
    }
}
```

- [ ] **Step 4: news.rs — RSS フィードからニュース取得**

```rust
pub struct NewsFetcher {
    client: reqwest::Client,
    sources: Vec<String>,
}

pub struct NewsItem {
    pub title: String,
    pub description: String,
    pub published: Option<chrono::DateTime<chrono::Utc>>,
    pub source: String,
}

impl NewsFetcher {
    pub fn new(sources: Vec<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            sources,
        }
    }

    pub async fn fetch_latest(&self) -> Vec<NewsItem> {
        let mut items = Vec::new();
        for source in &self.sources {
            match self.fetch_feed(source).await {
                Ok(mut feed_items) => items.append(&mut feed_items),
                Err(e) => tracing::warn!("failed to fetch news from {source}: {e}"),
            }
        }
        items
    }

    async fn fetch_feed(&self, url: &str) -> anyhow::Result<Vec<NewsItem>> {
        let body = self.client.get(url).send().await?.text().await?;
        let feed = feed_rs::parser::parse(body.as_bytes())?;
        let items = feed.entries.into_iter().take(10).map(|entry| {
            NewsItem {
                title: entry.title.map(|t| t.content).unwrap_or_default(),
                description: entry.summary.map(|s| s.content).unwrap_or_default(),
                published: entry.published.map(|d| d.into()),
                source: url.to_string(),
            }
        }).collect();
        Ok(items)
    }
}
```

- [ ] **Step 5: calendar.rs — 経済指標カレンダー取得**

無料 API としては Forex Factory の HTML パースが一般的だが、壊れやすいため最小実装とする。Phase 0 では RSS ベースの簡易実装から始める:

```rust
pub struct EconomicCalendar {
    client: reqwest::Client,
}

pub struct EconomicEvent {
    pub title: String,
    pub currency: String,
    pub impact: String,  // high / medium / low
    pub datetime: chrono::DateTime<chrono::Utc>,
}

impl EconomicCalendar {
    pub fn new() -> Self {
        Self { client: reqwest::Client::new() }
    }

    /// Fetch upcoming high-impact events.
    /// Phase 0: uses DailyFX economic calendar RSS or similar free source.
    /// Returns parsed events or empty vec on failure.
    pub async fn fetch_upcoming(&self) -> Vec<EconomicEvent> {
        // Implementation depends on chosen source.
        // Fallback: return empty vec and log warning.
        // The actual source selection should be done during implementation
        // based on what's accessible and stable at build time.
        tracing::warn!("economic calendar: using stub implementation");
        Vec::new()
    }
}
```

Note: 経済指標カレンダーの具体的な情報源は実装時に選定する。design.md に「情報源は実用性で選定し、固定しない」とあるため、実装者が利用可能で安定した無料ソースを選ぶこと。

- [ ] **Step 6: analyst.rs — オーケストレータ**

```rust
use auto_trader_core::strategy::MacroUpdate;
use crate::calendar::EconomicCalendar;
use crate::news::{NewsFetcher, NewsItem};
use crate::summarizer::GeminiSummarizer;

pub struct MacroAnalyst {
    calendar: EconomicCalendar,
    news: NewsFetcher,
    summarizer: GeminiSummarizer,
    vegapunk: Option<auto_trader_vegapunk::client::VegapunkClient>,
    pool: Option<sqlx::PgPool>,
}

impl MacroAnalyst {
    // ... constructor ...

    /// Run the macro analyst loop. Periodically fetches news and calendar,
    /// summarizes via Gemini, stores in DB and Vegapunk, returns MacroUpdate.
    pub async fn run(
        &mut self,
        macro_tx: tokio::sync::broadcast::Sender<MacroUpdate>,
        news_interval: std::time::Duration,
    ) -> anyhow::Result<()> {
        let mut tick = tokio::time::interval(news_interval);
        loop {
            tick.tick().await;

            // Fetch news
            let news_items = self.news.fetch_latest().await;
            for item in &news_items {
                let summary = match self.summarizer.summarize_for_fx(
                    &format!("{}: {}", item.title, item.description)
                ).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("summarization failed: {e}");
                        continue;
                    }
                };

                // Ingest to Vegapunk
                if let Some(vp) = &mut self.vegapunk {
                    let timestamp = chrono::Utc::now().to_rfc3339();
                    if let Err(e) = vp.ingest_raw(
                        &summary, "market_event", "macro-events", &timestamp
                    ).await {
                        tracing::warn!("vegapunk ingest failed: {e}");
                    }
                }

                // Store in DB
                if let Some(pool) = &self.pool {
                    let _ = auto_trader_db::macro_events::insert_macro_event(
                        pool, &summary, "news", "medium",
                        chrono::Utc::now(), Some(&item.source),
                    ).await;
                }

                // Broadcast to strategies
                let update = MacroUpdate {
                    summary: summary.clone(),
                    adjustments: std::collections::HashMap::new(),
                };
                let _ = macro_tx.send(update);
            }
        }
    }
}
```

- [ ] **Step 7: workspace + Cargo.toml**

`Cargo.toml` workspace members に `"crates/macro-analyst"` を追加。

`crates/macro-analyst/Cargo.toml`:

```toml
[package]
name = "auto-trader-macro-analyst"
version = "0.1.0"
edition.workspace = true

[dependencies]
auto-trader-core = { workspace = true }
auto-trader-db = { workspace = true }
auto-trader-vegapunk = { workspace = true }
reqwest = { workspace = true }
tokio = { workspace = true }
chrono = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
feed-rs = "2"
sqlx = { workspace = true }
```

`[workspace.dependencies]` に追加:

```toml
auto-trader-macro-analyst = { path = "crates/macro-analyst" }
```

- [ ] **Step 8: テスト・ビルド確認**

Run: `docker run --rm -v "$(pwd):/app" -w /app rust:1.85-bookworm cargo build --workspace`
Expected: ビルド成功

- [ ] **Step 9: Commit**

```bash
git add crates/macro-analyst/ crates/db/src/macro_events.rs crates/db/src/lib.rs crates/core/src/config.rs Cargo.toml
git commit -m "feat: add macro-analyst crate with news/calendar + Gemini Flash summarization

Fetches FX news via RSS, summarizes with Gemini Flash, stores in DB
and Vegapunk via IngestRaw. Broadcasts MacroUpdate to strategies."
```

---

### Task 6: SwingLLMv1 Strategy

**Files:**
- Create: `crates/strategy/src/swing_llm.rs`
- Modify: `crates/strategy/src/lib.rs`
- Modify: `crates/strategy/Cargo.toml`
- Modify: `crates/app/src/main.rs`

**Goal:** Vegapunk Search + Gemini Flash で判断するスイングトレード戦略を実装する。

- [ ] **Step 1: swing_llm.rs を作成**

```rust
use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{MacroUpdate, Strategy};
use auto_trader_core::types::{Direction, Pair, Signal};
use auto_trader_vegapunk::client::VegapunkClient;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::Mutex;

pub struct SwingLLMv1 {
    name: String,
    pairs: Vec<Pair>,
    holding_days_max: u32,
    vegapunk: Mutex<VegapunkClient>,
    gemini_api_url: String,
    gemini_api_key: String,
    gemini_model: String,
    last_check: HashMap<String, chrono::DateTime<chrono::Utc>>,
    check_interval: chrono::Duration,
    latest_macro: Option<String>,
}

impl SwingLLMv1 {
    pub fn new(
        name: String,
        pairs: Vec<Pair>,
        holding_days_max: u32,
        vegapunk: VegapunkClient,
        gemini_api_url: String,
        gemini_api_key: String,
        gemini_model: String,
    ) -> Self {
        Self {
            name,
            pairs,
            holding_days_max,
            vegapunk: Mutex::new(vegapunk),
            gemini_api_url,
            gemini_api_key,
            gemini_model,
            last_check: HashMap::new(),
            check_interval: chrono::Duration::hours(4), // Check every 4 hours
            latest_macro: None,
        }
    }

    async fn should_check(&mut self, pair: &str) -> bool {
        let now = chrono::Utc::now();
        match self.last_check.get(pair) {
            Some(last) => now - *last >= self.check_interval,
            None => true,
        }
    }

    async fn query_vegapunk_and_llm(
        &self,
        pair: &Pair,
        current_price: Decimal,
    ) -> anyhow::Result<Option<(Direction, Decimal, Decimal, Decimal)>> {
        // 1. Search Vegapunk for similar patterns
        let query = format!(
            "{}の現在の市場状況とトレード判断。価格: {}",
            pair.0, current_price
        );
        let mut vp = self.vegapunk.lock().await;
        let search_result = vp.search(&query, "local", 5).await?;

        // 2. Build context from search results
        let context: Vec<String> = search_result.results.iter()
            .filter_map(|r| r.text.clone())
            .collect();

        // 3. Ask Gemini Flash for trade decision
        let macro_context = self.latest_macro.as_deref().unwrap_or("マクロ情報なし");
        let prompt = format!(
            "あなたはFXスイングトレードの判断AIです。以下の情報からトレード判断をしてください。\n\n\
             通貨ペア: {}\n現在価格: {}\n\n\
             過去の類似判断:\n{}\n\n\
             マクロ環境: {}\n\n\
             回答は必ず以下のJSON形式のみで返してください:\n\
             {{\"action\": \"long\" | \"short\" | \"none\", \"confidence\": 0.0-1.0, \
             \"sl_pips\": number, \"tp_pips\": number, \"reason\": \"string\"}}",
            pair.0, current_price,
            context.join("\n"),
            macro_context,
        );

        let client = reqwest::Client::new();
        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            self.gemini_api_url, self.gemini_model, self.gemini_api_key
        );

        let body = serde_json::json!({
            "contents": [{"parts": [{"text": prompt}]}]
        });

        let resp: serde_json::Value = client
            .post(&url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let text = resp["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("");

        // Parse JSON response from LLM
        // Strip markdown code fences if present
        let json_text = text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let decision: serde_json::Value = serde_json::from_str(json_text)?;

        let action = decision["action"].as_str().unwrap_or("none");
        let confidence = decision["confidence"].as_f64().unwrap_or(0.0);
        let sl_pips = decision["sl_pips"].as_f64().unwrap_or(100.0);
        let tp_pips = decision["tp_pips"].as_f64().unwrap_or(200.0);

        if action == "none" || confidence < 0.6 {
            return Ok(None);
        }

        let direction = match action {
            "long" => Direction::Long,
            "short" => Direction::Short,
            _ => return Ok(None),
        };

        let pip_size = if current_price > Decimal::from(10) {
            Decimal::new(1, 2) // JPY pairs
        } else {
            Decimal::new(1, 4)
        };

        let sl = pip_size * Decimal::try_from(sl_pips)?;
        let tp = pip_size * Decimal::try_from(tp_pips)?;

        Ok(Some((direction, current_price, sl, tp)))
    }
}

#[async_trait::async_trait]
impl Strategy for SwingLLMv1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }

        let pair_key = event.pair.0.clone();
        if !self.should_check(&pair_key).await {
            return None;
        }
        self.last_check.insert(pair_key, chrono::Utc::now());

        let result = self.query_vegapunk_and_llm(&event.pair, event.candle.close).await;
        match result {
            Ok(Some((direction, entry, sl, tp))) => {
                let (stop_loss, take_profit) = match direction {
                    Direction::Long => (entry - sl, entry + tp),
                    Direction::Short => (entry + sl, entry - tp),
                };
                Some(Signal {
                    strategy_name: self.name.clone(),
                    pair: event.pair.clone(),
                    direction,
                    entry_price: entry,
                    stop_loss,
                    take_profit,
                    confidence: 0.6,
                    timestamp: event.timestamp,
                })
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("swing_llm decision failed for {}: {e}", event.pair);
                None
            }
        }
    }

    fn on_macro_update(&mut self, update: &MacroUpdate) {
        self.latest_macro = Some(update.summary.clone());
    }
}
```

- [ ] **Step 2: lib.rs と Cargo.toml を更新**

`crates/strategy/src/lib.rs`:
```rust
pub mod engine;
pub mod trend_follow;
pub mod swing_llm;
```

`crates/strategy/Cargo.toml` に依存追加:

```toml
auto-trader-vegapunk = { workspace = true }
reqwest = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
```

- [ ] **Step 3: main.rs に swing_llm_v1 の登録を追加**

```rust
name if name.starts_with("swing_llm") => {
    let holding_days_max = sc.params.get("holding_days_max")
        .and_then(|v| v.as_integer()).unwrap_or(14) as u32;
    let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();

    let gemini_api_key = std::env::var("GEMINI_API_KEY")
        .expect("GEMINI_API_KEY must be set for swing_llm strategy");
    let gemini_config = config.gemini.as_ref()
        .expect("gemini config required for swing_llm");

    let vp_config = &config.vegapunk;
    let vp_client = auto_trader_vegapunk::client::VegapunkClient::connect(
        &vp_config.endpoint, &vp_config.schema
    ).await?;

    engine.add_strategy(
        Box::new(auto_trader_strategy::swing_llm::SwingLLMv1::new(
            sc.name.clone(),
            pairs,
            holding_days_max,
            vp_client,
            gemini_config.api_url.clone(),
            gemini_api_key,
            gemini_config.model.clone(),
        )),
        sc.mode.clone(),
    );
    tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
}
```

- [ ] **Step 4: config/default.toml に swing_llm_v1 と gemini を追加**

```toml
[gemini]
model = "gemini-2.0-flash"
api_url = "https://generativelanguage.googleapis.com"

[[strategies]]
name = "swing_llm_v1"
enabled = true
mode = "paper"
pairs = ["USD_JPY", "EUR_USD"]
params = { holding_days_max = 14 }
```

- [ ] **Step 5: テスト・ビルド**

Run: `docker run --rm -v "$(pwd):/app" -w /app rust:1.85-bookworm cargo build --workspace`
Expected: ビルド成功

- [ ] **Step 6: Commit**

```bash
git add crates/strategy/ crates/app/src/main.rs config/default.toml
git commit -m "feat: add swing_llm_v1 strategy with Vegapunk Search + Gemini Flash

Checks every 4 hours per pair. Searches Vegapunk for similar patterns,
asks Gemini Flash for trade decision with confidence score. Only acts
on confidence >= 0.6. Incorporates macro updates from macro-analyst."
```

---

### Task 7: Backtest Crate

**Files:**
- Create: `crates/backtest/Cargo.toml`
- Create: `crates/backtest/src/lib.rs`
- Create: `crates/backtest/src/runner.rs`
- Create: `crates/backtest/src/report.rs`
- Modify: `Cargo.toml` (workspace)

**Goal:** price_candles テーブルから過去データを読み込み、PriceEvent を時系列順に Strategy に流してバックテストする。短期ルールベース戦略のみ対象。

- [ ] **Step 1: workspace に追加**

`Cargo.toml` workspace members に `"crates/backtest"` を追加。

`crates/backtest/Cargo.toml`:

```toml
[package]
name = "auto-trader-backtest"
version = "0.1.0"
edition.workspace = true

[dependencies]
auto-trader-core = { workspace = true }
auto-trader-db = { workspace = true }
auto-trader-market = { workspace = true }
auto-trader-executor = { workspace = true }
tokio = { workspace = true }
chrono = { workspace = true }
rust_decimal = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
sqlx = { workspace = true }
uuid = { workspace = true }
```

- [ ] **Step 2: runner.rs を作成**

```rust
use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{Candle, Direction, ExitReason, Pair, Trade, TradeMode, TradeStatus};
use auto_trader_executor::paper::PaperTrader;
use auto_trader_core::executor::OrderExecutor;
use crate::report::BacktestReport;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;

pub struct BacktestRunner {
    pool: sqlx::PgPool,
}

impl BacktestRunner {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    pub async fn run(
        &self,
        strategy: &mut dyn Strategy,
        pair: &Pair,
        timeframe: &str,
        initial_balance: Decimal,
        leverage: Decimal,
    ) -> anyhow::Result<BacktestReport> {
        // Load candles from DB
        let candles = auto_trader_db::candles::get_candles(
            &self.pool, &pair.0, timeframe, 10000
        ).await?;

        if candles.is_empty() {
            anyhow::bail!("no candle data for {} {}", pair, timeframe);
        }

        let trader = Arc::new(PaperTrader::new(initial_balance, leverage));
        let mut trades: Vec<Trade> = Vec::new();

        // Replay candles chronologically
        for (i, candle) in candles.iter().enumerate() {
            // Build indicators from available history
            let closes: Vec<Decimal> = candles[..=i].iter().map(|c| c.close).collect();
            let mut indicators = HashMap::new();
            if let Some(v) = auto_trader_market::indicators::sma(&closes, 20) {
                indicators.insert("sma_20".to_string(), v);
            }
            if let Some(v) = auto_trader_market::indicators::sma(&closes, 50) {
                indicators.insert("sma_50".to_string(), v);
            }
            if let Some(v) = auto_trader_market::indicators::rsi(&closes, 14) {
                indicators.insert("rsi_14".to_string(), v);
            }

            let event = PriceEvent {
                pair: pair.clone(),
                candle: candle.clone(),
                indicators,
                timestamp: candle.timestamp,
            };

            // Check SL/TP on open positions
            let positions = trader.open_positions().await?;
            for pos in positions {
                let t = &pos.trade;
                if t.pair != *pair { continue; }
                let exit = match t.direction {
                    Direction::Long => {
                        if candle.low <= t.stop_loss { Some((ExitReason::SlHit, t.stop_loss)) }
                        else if candle.high >= t.take_profit { Some((ExitReason::TpHit, t.take_profit)) }
                        else { None }
                    }
                    Direction::Short => {
                        if candle.high >= t.stop_loss { Some((ExitReason::SlHit, t.stop_loss)) }
                        else if candle.low <= t.take_profit { Some((ExitReason::TpHit, t.take_profit)) }
                        else { None }
                    }
                };
                if let Some((reason, price)) = exit {
                    let closed = trader.close_position(&t.id.to_string(), reason, price).await?;
                    trades.push(closed);
                }
            }

            // Run strategy
            if let Some(signal) = strategy.on_price(&event).await {
                // Check 1-pair-1-position
                let open = trader.open_positions().await?;
                let has_pos = open.iter().any(|p| {
                    p.trade.strategy_name == signal.strategy_name && p.trade.pair == signal.pair
                });
                if !has_pos {
                    if let Ok(trade) = trader.execute(&signal).await {
                        trades.push(trade);
                    }
                }
            }
        }

        let final_balance = trader.balance().await;
        Ok(BacktestReport::from_trades(trades, initial_balance, final_balance))
    }
}
```

- [ ] **Step 3: report.rs を作成**

```rust
use auto_trader_core::types::{Trade, TradeStatus};
use rust_decimal::Decimal;

pub struct BacktestReport {
    pub total_trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: f64,
    pub total_pnl: Decimal,
    pub max_drawdown: Decimal,
    pub initial_balance: Decimal,
    pub final_balance: Decimal,
    pub profit_factor: f64,
}

impl BacktestReport {
    pub fn from_trades(trades: Vec<Trade>, initial_balance: Decimal, final_balance: Decimal) -> Self {
        let closed: Vec<&Trade> = trades.iter()
            .filter(|t| t.status == TradeStatus::Closed)
            .collect();

        let total_trades = closed.len();
        let wins = closed.iter().filter(|t| t.pnl_pips.unwrap_or_default() > Decimal::ZERO).count();
        let losses = total_trades - wins;
        let win_rate = if total_trades > 0 { wins as f64 / total_trades as f64 } else { 0.0 };

        let total_pnl = closed.iter()
            .filter_map(|t| t.pnl_amount)
            .sum::<Decimal>();

        // Max drawdown from equity curve
        let mut peak = initial_balance;
        let mut max_dd = Decimal::ZERO;
        let mut equity = initial_balance;
        for t in &closed {
            equity += t.pnl_amount.unwrap_or_default();
            if equity > peak { peak = equity; }
            let dd = peak - equity;
            if dd > max_dd { max_dd = dd; }
        }

        let gross_profit: Decimal = closed.iter()
            .filter_map(|t| t.pnl_amount)
            .filter(|p| *p > Decimal::ZERO)
            .sum();
        let gross_loss: Decimal = closed.iter()
            .filter_map(|t| t.pnl_amount)
            .filter(|p| *p < Decimal::ZERO)
            .map(|p| p.abs())
            .sum();
        let profit_factor = if gross_loss > Decimal::ZERO {
            (gross_profit / gross_loss).to_string().parse().unwrap_or(0.0)
        } else if gross_profit > Decimal::ZERO {
            f64::INFINITY
        } else {
            0.0
        };

        Self {
            total_trades, wins, losses, win_rate,
            total_pnl, max_drawdown: max_dd,
            initial_balance, final_balance, profit_factor,
        }
    }

    pub fn print_summary(&self) {
        println!("=== Backtest Report ===");
        println!("Trades: {} (W:{} L:{})", self.total_trades, self.wins, self.losses);
        println!("Win Rate: {:.1}%", self.win_rate * 100.0);
        println!("Total PnL: {}", self.total_pnl);
        println!("Max Drawdown: {}", self.max_drawdown);
        println!("Profit Factor: {:.2}", self.profit_factor);
        println!("Balance: {} → {}", self.initial_balance, self.final_balance);
    }
}
```

- [ ] **Step 4: lib.rs**

```rust
pub mod runner;
pub mod report;
```

- [ ] **Step 5: ビルド確認**

Run: `docker run --rm -v "$(pwd):/app" -w /app rust:1.85-bookworm cargo build --workspace`
Expected: ビルド成功

- [ ] **Step 6: Commit**

```bash
git add crates/backtest/ Cargo.toml
git commit -m "feat: add backtest crate for historical strategy validation

Replays price_candles from DB through Strategy trait with PaperTrader.
Calculates win rate, PnL, max drawdown, profit factor.
Short-term rule-based strategies only (swing excluded per design)."
```

---

### Task 8: Max Drawdown Daily Batch

**Files:**
- Modify: `crates/db/src/summary.rs`
- Modify: `crates/app/src/main.rs`

**Goal:** UTC 0:00 に日次 max_drawdown を計算して daily_summary を更新する。

- [ ] **Step 1: summary.rs に max_drawdown 計算関数を追加**

```rust
pub async fn update_daily_max_drawdown(
    pool: &PgPool,
    date: chrono::NaiveDate,
) -> anyhow::Result<()> {
    // Get all closed trades for the date, ordered by exit_at
    let rows: Vec<(String, String, String, rust_decimal::Decimal)> = sqlx::query_as(
        "SELECT strategy_name, pair, mode, pnl_amount
         FROM trades
         WHERE status = 'closed' AND DATE(exit_at) = $1
         ORDER BY exit_at ASC"
    )
    .bind(date)
    .fetch_all(pool)
    .await?;

    // Group by (strategy, pair, mode) and calculate max drawdown per group
    let mut groups: std::collections::HashMap<(String, String, String), Vec<rust_decimal::Decimal>> = std::collections::HashMap::new();
    for (strategy, pair, mode, pnl) in rows {
        groups.entry((strategy, pair, mode)).or_default().push(pnl);
    }

    for ((strategy, pair, mode), pnls) in groups {
        let mut peak = rust_decimal::Decimal::ZERO;
        let mut equity = rust_decimal::Decimal::ZERO;
        let mut max_dd = rust_decimal::Decimal::ZERO;
        for pnl in pnls {
            equity += pnl;
            if equity > peak { peak = equity; }
            let dd = peak - equity;
            if dd > max_dd { max_dd = dd; }
        }

        sqlx::query(
            "UPDATE daily_summary SET max_drawdown = $1
             WHERE date = $2 AND strategy_name = $3 AND pair = $4 AND mode = $5"
        )
        .bind(max_dd)
        .bind(date)
        .bind(&strategy)
        .bind(&pair)
        .bind(&mode)
        .execute(pool)
        .await?;
    }

    Ok(())
}
```

- [ ] **Step 2: main.rs に日次バッチタスクを追加**

```rust
// Task: Daily batch (max_drawdown calculation at UTC 0:00)
let daily_pool = pool.clone();
let daily_handle = tokio::spawn(async move {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
    let mut last_date = chrono::Utc::now().date_naive();
    loop {
        interval.tick().await;
        let today = chrono::Utc::now().date_naive();
        if today != last_date {
            // Date changed — calculate yesterday's max_drawdown
            tracing::info!("running daily batch for {last_date}");
            if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(
                &daily_pool, last_date
            ).await {
                tracing::error!("daily batch failed: {e}");
            }
            last_date = today;
        }
    }
});
```

- [ ] **Step 3: Commit**

```bash
git add crates/db/src/summary.rs crates/app/src/main.rs
git commit -m "feat: add daily max_drawdown batch calculation at UTC 0:00

Calculates cumulative PnL curve per strategy/pair/mode from closed
trades and updates daily_summary.max_drawdown at date rollover."
```

---

### Task 9: Vegapunk Ingestion in Recorder

**Files:**
- Modify: `crates/app/src/main.rs`

**Goal:** トレード判断時と決済時に Vegapunk IngestRaw を呼び出し、根拠を蓄積する。

- [ ] **Step 1: recorder タスクに Vegapunk ingestion を追加**

specs/vegapunk-integration.md のフォーマットに従い、executor と recorder に IngestRaw を追加:

```rust
// In executor task, after successful execute():
if let Some(vp) = &vegapunk_client {
    let mut vp = vp.lock().await;
    let text = format!(
        "{} {} 判断。trade_id: {}。エントリー価格: {}。SL: {}, TP: {}。戦略: {}",
        trade.pair, match trade.direction {
            Direction::Long => "ロング",
            Direction::Short => "ショート",
        },
        trade.id, trade.entry_price, trade.stop_loss, trade.take_profit,
        trade.strategy_name
    );
    let channel = format!("{}-trades", trade.pair.0.to_lowercase());
    let timestamp = chrono::Utc::now().to_rfc3339();
    if let Err(e) = vp.ingest_raw(&text, "trade_signal", &channel, &timestamp).await {
        tracing::warn!("vegapunk ingest failed for trade open: {e}");
    }
}
```

同様に recorder の Closed ハンドラに:

```rust
// After updating trade in DB
if let Some(vp) = &vegapunk_client {
    let mut vp = vp.lock().await;
    let text = format!(
        "{} {} 決済。trade_id: {}。{:?}。PnL: {} pips。保有時間: {}",
        t.pair, match t.direction {
            Direction::Long => "ロング",
            Direction::Short => "ショート",
        },
        t.id, exit_reason,
        pnl_pips,
        // Calculate holding time
        exit_at.signed_duration_since(t.entry_at)
    );
    let channel = format!("{}-trades", t.pair.0.to_lowercase());
    let timestamp = exit_at.to_rfc3339();
    if let Err(e) = vp.ingest_raw(&text, "trade_result", &channel, &timestamp).await {
        tracing::warn!("vegapunk ingest failed for trade close: {e}");
    }
}
```

- [ ] **Step 2: main.rs で VegapunkClient を初期化して共有**

```rust
// Vegapunk client (optional — continues without if unavailable)
let vegapunk_client: Option<Arc<Mutex<VegapunkClient>>> = match VegapunkClient::connect(
    &config.vegapunk.endpoint, &config.vegapunk.schema
).await {
    Ok(client) => {
        tracing::info!("vegapunk connected: {}", config.vegapunk.endpoint);
        Some(Arc::new(Mutex::new(client)))
    }
    Err(e) => {
        tracing::warn!("vegapunk unavailable (continuing without): {e}");
        None
    }
};
```

- [ ] **Step 3: Commit**

```bash
git add crates/app/src/main.rs
git commit -m "feat: add Vegapunk IngestRaw for trade decisions and results

Ingests trade open/close events to Vegapunk for knowledge accumulation.
Connection is optional — system continues without Vegapunk if unavailable."
```

---

### Task 10: Pipeline Integration & E2E Verification

**Files:**
- Modify: `config/default.toml`
- Modify: `docker-compose.yml`
- Modify: `.env.example`
- Modify: `Dockerfile`
- Create: `schemas/fx-trading.yml`

**Goal:** 全コンポーネントを結合し、docker-compose up で 24/365 ペーパートレードが動く状態にする。

- [ ] **Step 1: config/default.toml を完成**

```toml
[oanda]
api_url = "https://api-fxpractice.oanda.com"
account_id = ""

[vegapunk]
endpoint = "http://fuj11-agent-01:3000"
schema = "fx-trading"

[database]
url = "postgresql://auto-trader:auto-trader@db:5432/auto_trader"

[monitor]
interval_secs = 60

[pairs]
active = ["USD_JPY", "EUR_USD"]

[gemini]
model = "gemini-2.0-flash"
api_url = "https://generativelanguage.googleapis.com"

[macro_analyst]
enabled = true
calendar_interval_secs = 3600
news_interval_secs = 1800
news_sources = [
    "https://www.forexlive.com/feed",
    "https://www.dailyfx.com/feeds/market-news",
]

[[strategies]]
name = "trend_follow_v1"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]
params = { ma_short = 20, ma_long = 50, rsi_threshold = 70 }

[[strategies]]
name = "swing_llm_v1"
enabled = true
mode = "paper"
pairs = ["USD_JPY", "EUR_USD"]
params = { holding_days_max = 14 }
```

- [ ] **Step 2: .env.example を更新**

```
OANDA_API_KEY=your-oanda-api-key-here
OANDA_ACCOUNT_ID=your-account-id-here
GEMINI_API_KEY=your-gemini-api-key-here
```

- [ ] **Step 3: docker-compose.yml を更新**

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
      GEMINI_API_KEY: ${GEMINI_API_KEY}
      RUST_LOG: info
    volumes:
      - ./config:/app/config:ro
    restart: unless-stopped

volumes:
  pgdata:
```

- [ ] **Step 4: Dockerfile に protoc を追加**

builder ステージに `protobuf-compiler` を追加:

```dockerfile
FROM rust:1.85-bookworm AS builder
RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY proto/ proto/
COPY crates/ crates/
COPY migrations/ migrations/
RUN cargo build --release --bin auto-trader
```

- [ ] **Step 5: schemas/fx-trading.yml を作成**

vegapunk-integration.md の定義に従って作成:

```yaml
# Vegapunk schema for FX trading knowledge graph
# See specs/vegapunk-integration.md for details

nodes:
  TradeDecision:
    attributes:
      pair: { type: string, required: true }
      direction: { type: string, required: true }
      entry_price: { type: string }
      stop_loss: { type: string }
      take_profit: { type: string }
      confidence: { type: string }
      decided_at: { type: string }

  MarketAnalysis:
    attributes:
      summary: { type: string, required: true }
      timeframe: { type: string }
      analysis_type: { type: string }

  TradeResult:
    attributes:
      summary: { type: string, required: true }
      pnl_pips: { type: string }
      exit_reason: { type: string }
      holding_time: { type: string }

  MarketEvent:
    attributes:
      summary: { type: string, required: true }
      event_type: { type: string }
      impact: { type: string }

  Strategy:
    attributes:
      name: { type: string, required: true }
      description: { type: string }
      version: { type: string }

edges:
  BASED_ON: { from: TradeDecision, to: MarketAnalysis }
  TRIGGERED_BY: { from: TradeDecision, to: MarketEvent }
  RESULTED_IN: { from: TradeDecision, to: TradeResult }
  USED_STRATEGY: { from: TradeDecision, to: Strategy }
  CONTRADICTS: { from: MarketAnalysis, to: MarketAnalysis }
  SUPERSEDES: { from: TradeDecision, to: TradeDecision }

traceable_pairs:
  - claim: TradeDecision
    evidence: MarketAnalysis
    edge: BASED_ON
  - claim: TradeDecision
    evidence: TradeResult
    edge: RESULTED_IN
```

- [ ] **Step 6: 全体ビルド確認**

Run: `docker build -t auto-trader .`
Expected: ビルド成功

- [ ] **Step 7: テスト実行**

Run: `docker run --rm -v "$(pwd):/app" -w /app rust:1.85-bookworm bash -c "apt-get update && apt-get install -y protobuf-compiler && cargo test --workspace"`
Expected: 全テスト PASS

- [ ] **Step 8: Commit**

```bash
git add config/ docker-compose.yml .env.example Dockerfile schemas/
git commit -m "feat: complete Phase 0 pipeline integration

- Full config with macro_analyst, gemini, swing_llm_v1
- docker-compose with restart policy and all env vars
- Dockerfile with protobuf-compiler for vegapunk client
- Vegapunk fx-trading schema definition"
```

---

## Post-Implementation: Dashboard (Plan 3)

Dashboard（dashboard-api + dashboard-ui）は別プランで実装する。トレードループが稼働し DB にデータが蓄積され始めてから着手する。

---

## Self-Review Checklist

- [x] **Spec coverage**: design.md Phase 0 スコープの全項目にタスクが対応
  - ✅ SL/TP 監視 → Task 2
  - ✅ Candle DB 保存 → Task 3
  - ✅ vegapunk-client → Task 4
  - ✅ macro-analyst → Task 5
  - ✅ swing_llm_v1 → Task 6
  - ✅ backtest → Task 7
  - ✅ max_drawdown → Task 8
  - ✅ Vegapunk ingestion → Task 9
  - ✅ Stability fixes → Task 1
  - ✅ Pipeline integration → Task 10
  - Dashboard → 別プラン（合意済み）
- [x] **Placeholder scan**: TBD/TODO なし。calendar.rs のみ stub 実装だが design.md に「情報源は実用性で選定」と明記されており、実装者が選定する旨を記載済み
- [x] **Type consistency**: VegapunkClient, MacroUpdate, Signal, Trade 等の型名はコードベース既存と一致
