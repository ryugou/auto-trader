# Vegapunk KnowledgeStore Abstraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ports-and-adapters 構造で Vegapunk gRPC クライアントを抽象化する。consumer は `Arc<dyn KnowledgeStore>` のみに依存し、production code パスへ mock を注入可能にし、`Mutex<VegapunkClient>` を撤廃する。

**Architecture:** `core` に 2 つの trait (`VegapunkApi` 低レベル / `KnowledgeStore` 高レベル)。`vegapunk-client::VegapunkClient` が `VegapunkApi` を実装、`app::knowledge::VegapunkKnowledgeStore` が `KnowledgeStore` を実装し `enriched_ingest` 整形を内側に集約する。

**Tech Stack:** Rust stable / tokio / tonic / async-trait (workspace 共通) / sqlx (DB) / 既存ワークスペース crate

---

## Required Test Command (each task の DoD)

CLAUDE.md 必須:

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
```

このコマンドは smoke_test / phase1 / phase2 / phase3 / phase4 全てを含む。**各タスクの末尾でこれを必ず実行し、全 pass を確認するまで次タスクへ進まない。** 加えて以下も各タスクの DoD:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace        # unit tests
```

---

## File Structure

新規:
- `crates/core/src/vegapunk_port.rs` — `VegapunkApi` trait + `SearchHit` / `SearchResults` / `SearchMode`
- `crates/core/src/knowledge.rs` — `KnowledgeStore` trait + `PatternHit` / `PatternSearchResults` / `MarketEvent` / `TradeCloseContext`
- `crates/app/src/knowledge.rs` — `VegapunkKnowledgeStore` 高レベル実装

変更:
- `crates/core/src/lib.rs` — モジュール宣言追加
- `crates/vegapunk-client/src/client.rs` — `VegapunkApi for VegapunkClient` 実装追加、旧 `&mut self` メソッド削除 (Task 10 で)
- `crates/app/src/lib.rs` / `crates/app/src/main.rs` — モジュール宣言と起動時配線
- `crates/app/src/main.rs` — トレード open/close フックを `Arc<dyn KnowledgeStore>` 経由に
- `crates/strategy/src/swing_llm.rs` — `Mutex<VegapunkClient>` → `Arc<dyn KnowledgeStore>`
- `crates/app/src/weekly_batch.rs` — 同上
- `crates/integration-tests/src/mocks/vegapunk.rs` — `MockVegapunk` → `MockVegapunkApi` (rename + `VegapunkApi` 実装)、TODO コメント削除
- `crates/integration-tests/tests/phase3_swing_llm.rs` — `Arc<dyn KnowledgeStore>` 注入に書き換え

---

## Task 0: Baseline 確認 (どの変更にも入る前に必須)

**Files:** なし

- [ ] **Step 1: ベースラインのフル統合テストを green 状態で確認**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
```

Expected: 全 pass。失敗があればこの計画着手前に修正する (refactor の起点が壊れていれば検証不能)。

- [ ] **Step 2: 現状のスナップショット記録**

`cargo test -p auto-trader-integration-tests 2>&1 | tail -10` の最終サマリ (例: `test result: ok. 224 passed; 0 failed`) を計画ファイルに書き留めず、自身のメモに保持。各タスク後に同じ数字以上を維持しているか確認の基準にする。

---

## Task 1: `core::vegapunk_port` — 低レベル trait 追加

**Files:**
- Create: `crates/core/src/vegapunk_port.rs`
- Modify: `crates/core/src/lib.rs`
- Test: `crates/core/src/vegapunk_port.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: テスト先行 — trait が要求するシグネチャを満たすスタブが書けることを検証**

`crates/core/src/vegapunk_port.rs` 末尾に追加:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct StubApi;

    #[async_trait]
    impl VegapunkApi for StubApi {
        async fn ingest_raw(&self, _: &str, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn search(&self, _: &str, _: SearchMode, _: i32) -> anyhow::Result<SearchResults> {
            Ok(SearchResults { hits: vec![], search_id: String::new() })
        }
        async fn feedback(&self, _: &str, _: i32, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn merge(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn trait_is_object_safe_and_callable() {
        let api: std::sync::Arc<dyn VegapunkApi> = std::sync::Arc::new(StubApi);
        api.ingest_raw("t", "s", "c", "ts").await.unwrap();
        let r = api.search("q", SearchMode::Local, 5).await.unwrap();
        assert_eq!(r.hits.len(), 0);
        api.feedback("sid", 5, "c").await.unwrap();
        api.merge().await.unwrap();
        assert_eq!(SearchMode::Local.as_str(), "local");
        assert_eq!(SearchMode::Global.as_str(), "global");
        assert_eq!(SearchMode::Hybrid.as_str(), "hybrid");
    }
}
```

- [ ] **Step 2: テストを実行して fail することを確認**

```bash
cargo test -p auto-trader-core vegapunk_port
```

Expected: コンパイルエラー (型未定義)。

- [ ] **Step 3: trait と型を実装**

`crates/core/src/vegapunk_port.rs` 冒頭:

```rust
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub text: String,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub search_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Local,
    Global,
    Hybrid,
}

impl SearchMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchMode::Local => "local",
            SearchMode::Global => "global",
            SearchMode::Hybrid => "hybrid",
        }
    }
}

#[async_trait]
pub trait VegapunkApi: Send + Sync {
    async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<()>;

    async fn search(
        &self,
        query: &str,
        mode: SearchMode,
        top_k: i32,
    ) -> anyhow::Result<SearchResults>;

    async fn feedback(
        &self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()>;

    async fn merge(&self) -> anyhow::Result<()>;
}
```

`crates/core/src/lib.rs` に追加:

```rust
pub mod vegapunk_port;
```

- [ ] **Step 4: 単体テストを実行して pass**

```bash
cargo test -p auto-trader-core vegapunk_port
```

Expected: PASS。

- [ ] **Step 5: フル統合テスト**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。

- [ ] **Step 6: Commit**

```bash
git checkout -b feat/vegapunk-knowledge-store
git add crates/core/src/vegapunk_port.rs crates/core/src/lib.rs
git commit -m "feat(core): add VegapunkApi trait + supporting types"
```

---

## Task 2: `core::knowledge` — 高レベル trait 追加

**Files:**
- Create: `crates/core/src/knowledge.rs`
- Modify: `crates/core/src/lib.rs`
- Test: `crates/core/src/knowledge.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: テスト先行 — KnowledgeStore のスタブ実装が書ける**

`crates/core/src/knowledge.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Direction, ExitReason, Pair, Trade, TradeStatus};
    use async_trait::async_trait;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use uuid::Uuid;

    struct StubStore;

    #[async_trait]
    impl KnowledgeStore for StubStore {
        async fn record_trade_open(
            &self,
            _: &Trade,
            _: &HashMap<String, Decimal>,
            _: Option<Decimal>,
        ) -> anyhow::Result<()> { Ok(()) }

        async fn record_trade_close(
            &self,
            _: &Trade,
            _: &TradeCloseContext<'_>,
        ) -> anyhow::Result<()> { Ok(()) }

        async fn record_market_event(&self, _: &MarketEvent<'_>) -> anyhow::Result<()> { Ok(()) }

        async fn search_similar_patterns(
            &self,
            _: &Pair,
            _: Decimal,
            _: i32,
        ) -> anyhow::Result<PatternSearchResults> {
            Ok(PatternSearchResults { hits: vec![], search_id: String::new() })
        }

        async fn search_strategy_outcomes(
            &self,
            _: &str,
            _: i32,
        ) -> anyhow::Result<PatternSearchResults> {
            Ok(PatternSearchResults { hits: vec![], search_id: String::new() })
        }

        async fn submit_feedback(&self, _: &str, _: i32, _: &str) -> anyhow::Result<()> { Ok(()) }
        async fn run_merge(&self) -> anyhow::Result<()> { Ok(()) }
    }

    #[tokio::test]
    async fn trait_is_object_safe() {
        let store: std::sync::Arc<dyn KnowledgeStore> = std::sync::Arc::new(StubStore);
        let pair = Pair("USD_JPY".to_string());
        let res = store.search_similar_patterns(&pair, Decimal::new(15000, 2), 5).await.unwrap();
        assert!(res.hits.is_empty());
    }
}
```

- [ ] **Step 2: テストが fail することを確認**

```bash
cargo test -p auto-trader-core knowledge
```

Expected: コンパイルエラー (型未定義)。

- [ ] **Step 3: trait と型を実装**

`crates/core/src/knowledge.rs` 冒頭:

```rust
use crate::types::{Pair, Trade};
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PatternHit {
    pub text: String,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct PatternSearchResults {
    pub hits: Vec<PatternHit>,
    pub search_id: String,
}

#[derive(Debug, Clone)]
pub struct MarketEvent<'a> {
    pub summary: &'a str,
    pub event_type: &'a str,
    pub impact: &'a str,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// `record_trade_close` に必要なコンテキスト。caller が DB 等から取得して詰める。
/// `enriched_ingest::format_trade_close` の引数構成に追従。
#[derive(Debug, Clone, Copy)]
pub struct TradeCloseContext<'a> {
    pub entry_indicators: Option<&'a serde_json::Value>,
    pub account_balance: Option<Decimal>,
    pub account_initial: Option<Decimal>,
}

#[async_trait]
pub trait KnowledgeStore: Send + Sync {
    async fn record_trade_open(
        &self,
        trade: &Trade,
        indicators: &HashMap<String, Decimal>,
        allocation_pct: Option<Decimal>,
    ) -> anyhow::Result<()>;

    async fn record_trade_close(
        &self,
        trade: &Trade,
        ctx: &TradeCloseContext<'_>,
    ) -> anyhow::Result<()>;

    async fn record_market_event(&self, event: &MarketEvent<'_>) -> anyhow::Result<()>;

    async fn search_similar_patterns(
        &self,
        pair: &Pair,
        current_price: Decimal,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults>;

    async fn search_strategy_outcomes(
        &self,
        strategy_name: &str,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults>;

    async fn submit_feedback(
        &self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()>;

    async fn run_merge(&self) -> anyhow::Result<()>;
}
```

`crates/core/src/lib.rs` に追加:

```rust
pub mod knowledge;
```

- [ ] **Step 4: テスト pass を確認**

```bash
cargo test -p auto-trader-core knowledge
```

Expected: PASS。

- [ ] **Step 5: フル統合テスト + 静的解析**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/knowledge.rs crates/core/src/lib.rs
git commit -m "feat(core): add KnowledgeStore trait + domain context types"
```

---

## Task 3: `VegapunkClient` の `VegapunkApi` 実装

**Files:**
- Modify: `crates/vegapunk-client/src/client.rs`
- Modify: `crates/vegapunk-client/Cargo.toml` (依存追加が必要なら)

旧 `&mut self` メソッドはこの段階では削除しない (consumer 移行が終わる Task 10 で削除)。新しい `&self` 実装を並存させる。

- [ ] **Step 1: `vegapunk-client/Cargo.toml` の依存を確認**

```bash
grep -E "async-trait|auto-trader-core" crates/vegapunk-client/Cargo.toml
```

`async-trait = { workspace = true }` と `auto-trader-core = { path = "../core" }` がなければ依存セクションに追加する (Cargo.toml の `[dependencies]` 直下):

```toml
async-trait = { workspace = true }
auto-trader-core = { path = "../core" }
```

- [ ] **Step 2: `VegapunkApi for VegapunkClient` 実装を追加**

`crates/vegapunk-client/src/client.rs` 末尾に追加:

```rust
use async_trait::async_trait;
use auto_trader_core::vegapunk_port::{SearchHit, SearchMode, SearchResults, VegapunkApi};

#[async_trait]
impl VegapunkApi for VegapunkClient {
    async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<()> {
        let mut client = self.client.clone();
        let request = IngestRawRequest {
            text: text.to_string(),
            metadata: Some(IngestRawMetadata {
                source_type: source_type.to_string(),
                author: None,
                channel: Some(channel.to_string()),
                timestamp: Some(timestamp.to_string()),
            }),
            schema: self.schema.clone(),
        };
        client.ingest_raw(request).await?;
        Ok(())
    }

    async fn search(
        &self,
        query: &str,
        mode: SearchMode,
        top_k: i32,
    ) -> anyhow::Result<SearchResults> {
        let mut client = self.client.clone();
        let request = SearchRequest {
            text: query.to_string(),
            filter: None,
            depth: None,
            top_k: Some(top_k),
            format: None,
            mode: Some(mode.as_str().to_string()),
            schema: self.schema.clone(),
            offset: None,
            limit: None,
            structural_weight: None,
        };
        let response = client.search(request).await?.into_inner();
        let hits = response
            .results
            .into_iter()
            .map(|r| SearchHit {
                text: r.text.unwrap_or_default(),
                score: r.score.unwrap_or(0.0),
            })
            .collect();
        Ok(SearchResults { hits, search_id: response.search_id })
    }

    async fn feedback(
        &self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()> {
        let mut client = self.client.clone();
        let request = FeedbackRequest {
            search_id: search_id.to_string(),
            rating,
            comment: comment.to_string(),
        };
        client.feedback(request).await?;
        Ok(())
    }

    async fn merge(&self) -> anyhow::Result<()> {
        let mut client = self.client.clone();
        let request = MergeRequest {
            schema: self.schema.clone(),
        };
        client.merge(request).await?;
        Ok(())
    }
}
```

注: 既存の `&mut self` メソッド 4 つはそのまま残置 (consumer 移行完了まで)。

- [ ] **Step 3: ビルド確認**

```bash
cargo build -p auto-trader-vegapunk
```

Expected: ビルド成功。

- [ ] **Step 4: phase4 統合テスト (実 Vegapunk 接続) で `&self` 経路が通ることを確認**

`crates/integration-tests/tests/phase4_external.rs` を仮編集 — 既存テストの後ろに追加 (Task 末尾でコミットせず確認用、Step 6 で revert する):

```rust
#[tokio::test]
#[ignore = "requires real Vegapunk"]
async fn vegapunk_api_trait_smoke() {
    let token = std::env::var("VEGAPUNK_AUTH_TOKEN").ok();
    if token.is_none() {
        println!("Vegapunk: VEGAPUNK_AUTH_TOKEN not set — SKIPPED");
        return;
    }
    let endpoint = std::env::var("VEGAPUNK_ENDPOINT")
        .unwrap_or_else(|_| "http://vegapunk.local:6840".to_string());
    let client = auto_trader_vegapunk::client::VegapunkClient::connect(&endpoint, "fx-trading", token.as_deref()).await.unwrap();
    use auto_trader_core::vegapunk_port::{SearchMode, VegapunkApi};
    let api: std::sync::Arc<dyn VegapunkApi> = std::sync::Arc::new(client);
    let res = api.search("smoke", SearchMode::Local, 1).await;
    assert!(res.is_ok(), "search via trait failed: {:?}", res.err());
}
```

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  VEGAPUNK_AUTH_TOKEN="${VEGAPUNK_AUTH_TOKEN}" \
  cargo test -p auto-trader-integration-tests phase4_external vegapunk_api_trait_smoke -- --include-ignored
```

Expected: PASS (auth token 設定済みなら) または SKIPPED。fail なら trait 実装の不具合。

- [ ] **Step 5: フル統合テスト + 静的解析**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。

- [ ] **Step 6: Step 4 で追加した一時テストを revert してから commit**

```bash
git checkout -- crates/integration-tests/tests/phase4_external.rs
git add crates/vegapunk-client/src/client.rs crates/vegapunk-client/Cargo.toml
git commit -m "feat(vegapunk-client): impl VegapunkApi (parallel to legacy &mut self)"
```

---

## Task 4: `MockVegapunk` を `MockVegapunkApi` にリネーム + `VegapunkApi` 実装

**Files:**
- Modify: `crates/integration-tests/src/mocks/vegapunk.rs`
- Modify: 既存の `MockVegapunk` 参照箇所 (grep で全件特定)

- [ ] **Step 1: 既存参照箇所を grep**

```bash
grep -rn "MockVegapunk\b" crates/integration-tests/ 2>/dev/null
```

参照リストを把握 (rename 時にこれらを全て更新する)。

- [ ] **Step 2: テスト先行 — trait オブジェクトとして mock を渡せることを検証**

`crates/integration-tests/src/mocks/vegapunk.rs` 末尾に `#[cfg(test)] mod tests` 追加:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::vegapunk_port::{SearchMode, VegapunkApi};
    use std::sync::Arc;

    #[tokio::test]
    async fn mock_implements_vegapunk_api() {
        let mock: Arc<dyn VegapunkApi> = Arc::new(
            MockVegapunkApiBuilder::new()
                .with_search_results(vec![SearchResult { text: "hi".into(), score: 0.9 }])
                .build(),
        );
        mock.ingest_raw("t", "trade_signal", "usd_jpy-trades", "2026-01-01T00:00:00Z").await.unwrap();
        let r = mock.search("q", SearchMode::Local, 5).await.unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].text, "hi");
        mock.feedback("sid", 5, "ok").await.unwrap();
        mock.merge().await.unwrap();
    }
}
```

- [ ] **Step 3: テストが fail することを確認**

```bash
cargo test -p auto-trader-integration-tests --lib mock_implements_vegapunk_api
```

Expected: コンパイルエラー (型未定義 / trait 未実装)。

- [ ] **Step 4: rename + trait 実装**

`crates/integration-tests/src/mocks/vegapunk.rs` の冒頭 TODO コメント (1-2 行目) を削除。

ファイル全体で `MockVegapunk` → `MockVegapunkApi`、`MockVegapunkBuilder` → `MockVegapunkApiBuilder` に置換 (sed ではなく Edit で安全に):

```bash
# 確認のため (実行はEditで):
grep -c "MockVegapunk\b" crates/integration-tests/src/mocks/vegapunk.rs
grep -c "MockVegapunkBuilder\b" crates/integration-tests/src/mocks/vegapunk.rs
```

ファイル末尾の `#[cfg(test)] mod tests` の直前に `VegapunkApi` 実装を追加:

```rust
use auto_trader_core::vegapunk_port::{
    SearchHit, SearchMode, SearchResults, VegapunkApi,
};
use async_trait::async_trait;

#[async_trait]
impl VegapunkApi for MockVegapunkApi {
    async fn ingest_raw(
        &self,
        text: &str,
        source_type: &str,
        channel: &str,
        timestamp: &str,
    ) -> anyhow::Result<()> {
        // 既存の inherent method を呼ぶ (戻り値 IngestRawResult は破棄)
        MockVegapunkApi::ingest_raw(self, text, source_type, channel, timestamp).await?;
        Ok(())
    }

    async fn search(
        &self,
        query: &str,
        mode: SearchMode,
        top_k: i32,
    ) -> anyhow::Result<SearchResults> {
        let results = MockVegapunkApi::search(self, query, mode.as_str(), top_k).await?;
        let hits = results.into_iter().map(|r| SearchHit { text: r.text, score: r.score }).collect();
        Ok(SearchResults { hits, search_id: "mock-search-id".to_string() })
    }

    async fn feedback(&self, search_id: &str, rating: i32, comment: &str) -> anyhow::Result<()> {
        MockVegapunkApi::feedback(self, search_id, rating, comment).await
    }

    async fn merge(&self) -> anyhow::Result<()> {
        MockVegapunkApi::merge(self).await
    }
}
```

注: trait 実装内では既存 inherent method (`MockVegapunkApi::ingest_raw` 等) を再利用することで、call tracking / failure injection の挙動を保つ。

`crates/integration-tests/Cargo.toml` に `auto-trader-core = { path = "../core" }` がなければ追加 (`async-trait` は既存):

```bash
grep "auto-trader-core" crates/integration-tests/Cargo.toml
```

- [ ] **Step 5: Step 1 の参照箇所 (テストファイル等) を全て新名前に更新**

各ファイルを Edit で `MockVegapunk` → `MockVegapunkApi`、`MockVegapunkBuilder` → `MockVegapunkApiBuilder` に置換。

- [ ] **Step 6: テスト pass を確認**

```bash
cargo test -p auto-trader-integration-tests --lib mock_implements_vegapunk_api
```

Expected: PASS。

- [ ] **Step 7: フル統合テスト + 静的解析**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。**rename したテストが既存挙動を壊していないことの担保。**

- [ ] **Step 8: Commit**

```bash
git add crates/integration-tests/src/mocks/vegapunk.rs crates/integration-tests/Cargo.toml crates/integration-tests/tests/
git commit -m "test: rename MockVegapunk -> MockVegapunkApi + impl VegapunkApi trait"
```

---

## Task 5: `VegapunkKnowledgeStore` 高レベル実装

**Files:**
- Create: `crates/app/src/knowledge.rs`
- Modify: `crates/app/src/lib.rs`
- Test: `crates/app/src/knowledge.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: テスト先行**

`crates/app/src/knowledge.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::{Direction, Pair, Trade, TradeStatus};
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::sync::Arc;
    use uuid::Uuid;

    // 最小限の VegapunkApi mock (call capture 用)
    struct CaptureApi {
        captured: std::sync::Mutex<Vec<(String, String, String, String)>>,
    }

    #[async_trait::async_trait]
    impl auto_trader_core::vegapunk_port::VegapunkApi for CaptureApi {
        async fn ingest_raw(&self, text: &str, source_type: &str, channel: &str, timestamp: &str) -> anyhow::Result<()> {
            self.captured.lock().unwrap().push((text.into(), source_type.into(), channel.into(), timestamp.into()));
            Ok(())
        }
        async fn search(&self, _: &str, _: auto_trader_core::vegapunk_port::SearchMode, _: i32) -> anyhow::Result<auto_trader_core::vegapunk_port::SearchResults> {
            Ok(auto_trader_core::vegapunk_port::SearchResults { hits: vec![], search_id: "sid".into() })
        }
        async fn feedback(&self, _: &str, _: i32, _: &str) -> anyhow::Result<()> { Ok(()) }
        async fn merge(&self) -> anyhow::Result<()> { Ok(()) }
    }

    fn fixture_trade() -> Trade {
        // 既存テスト fixture と同じ構成
        Trade {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            strategy_name: "test_strategy".to_string(),
            pair: Pair("USD_JPY".to_string()),
            exchange: auto_trader_core::types::Exchange::GmoFx,
            direction: Direction::Long,
            entry_price: Decimal::new(15000, 2),
            entry_at: Utc::now(),
            stop_loss: Decimal::new(14900, 2),
            take_profit: Some(Decimal::new(15200, 2)),
            quantity: Decimal::new(1000, 0),
            status: TradeStatus::Open,
            pnl_amount: None,
            fees: Decimal::ZERO,
            exit_price: None,
            exit_at: None,
            exit_reason: None,
        }
    }

    #[tokio::test]
    async fn record_trade_open_emits_ingest_with_correct_metadata() {
        let api = Arc::new(CaptureApi { captured: std::sync::Mutex::new(Vec::new()) });
        let store = VegapunkKnowledgeStore::new(api.clone() as Arc<dyn auto_trader_core::vegapunk_port::VegapunkApi>);

        let mut indicators = HashMap::new();
        indicators.insert("rsi".to_string(), Decimal::new(50, 0));
        let trade = fixture_trade();

        store.record_trade_open(&trade, &indicators, Some(Decimal::new(2, 2))).await.unwrap();

        let captured = api.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let (text, source_type, channel, _ts) = &captured[0];
        assert_eq!(source_type, "trade_signal");
        assert_eq!(channel, "usd_jpy-trades");
        assert!(text.contains("USD_JPY"), "text should include pair");
        assert!(text.contains("ロング"), "text should include direction");
    }
}
```

注: `Trade` の構造体フィールドは `crates/core/src/types.rs:257` の現状定義に合わせる。Step 3 実装着手時に最新の `Trade` を Read で確認し、`fixture_trade` を辻褄合わせする。

- [ ] **Step 2: テスト fail 確認**

```bash
cargo test -p auto-trader record_trade_open_emits_ingest_with_correct_metadata
```

Expected: コンパイルエラー (型未定義)。

- [ ] **Step 3: 実装**

`crates/app/src/knowledge.rs` 冒頭:

```rust
use crate::enriched_ingest;
use async_trait::async_trait;
use auto_trader_core::knowledge::{
    KnowledgeStore, MarketEvent, PatternHit, PatternSearchResults, TradeCloseContext,
};
use auto_trader_core::types::{Pair, Trade};
use auto_trader_core::vegapunk_port::{SearchMode, VegapunkApi};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;

pub struct VegapunkKnowledgeStore {
    api: Arc<dyn VegapunkApi>,
}

impl VegapunkKnowledgeStore {
    pub fn new(api: Arc<dyn VegapunkApi>) -> Self { Self { api } }

    fn now_rfc3339() -> String { chrono::Utc::now().to_rfc3339() }

    fn pair_channel(pair: &Pair) -> String {
        format!("{}-trades", pair.0.to_lowercase())
    }
}

#[async_trait]
impl KnowledgeStore for VegapunkKnowledgeStore {
    async fn record_trade_open(
        &self,
        trade: &Trade,
        indicators: &HashMap<String, Decimal>,
        allocation_pct: Option<Decimal>,
    ) -> anyhow::Result<()> {
        let text = enriched_ingest::format_trade_open(trade, indicators, allocation_pct);
        let channel = Self::pair_channel(&trade.pair);
        let timestamp = Self::now_rfc3339();
        self.api.ingest_raw(&text, "trade_signal", &channel, &timestamp).await
    }

    async fn record_trade_close(
        &self,
        trade: &Trade,
        ctx: &TradeCloseContext<'_>,
    ) -> anyhow::Result<()> {
        let text = enriched_ingest::format_trade_close(
            trade,
            ctx.entry_indicators,
            ctx.account_balance,
            ctx.account_initial,
        );
        let channel = Self::pair_channel(&trade.pair);
        let timestamp = trade.exit_at
            .map(|e| e.to_rfc3339())
            .unwrap_or_else(Self::now_rfc3339);
        self.api.ingest_raw(&text, "trade_result", &channel, &timestamp).await
    }

    async fn record_market_event(&self, event: &MarketEvent<'_>) -> anyhow::Result<()> {
        let text = format!(
            "[{}] {} (impact={})",
            event.event_type, event.summary, event.impact
        );
        let timestamp = event.timestamp.to_rfc3339();
        self.api.ingest_raw(&text, "market_event", "macro-events", &timestamp).await
    }

    async fn search_similar_patterns(
        &self,
        pair: &Pair,
        current_price: Decimal,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults> {
        let query = format!(
            "{}の現在の市場状況とトレード判断。価格: {}",
            pair.0, current_price
        );
        let res = self.api.search(&query, SearchMode::Local, top_k).await?;
        Ok(PatternSearchResults {
            hits: res.hits.into_iter().map(|h| PatternHit { text: h.text, score: h.score }).collect(),
            search_id: res.search_id,
        })
    }

    async fn search_strategy_outcomes(
        &self,
        strategy_name: &str,
        top_k: i32,
    ) -> anyhow::Result<PatternSearchResults> {
        let query = format!("{}戦略の勝率と傾向", strategy_name);
        let res = self.api.search(&query, SearchMode::Hybrid, top_k).await?;
        Ok(PatternSearchResults {
            hits: res.hits.into_iter().map(|h| PatternHit { text: h.text, score: h.score }).collect(),
            search_id: res.search_id,
        })
    }

    async fn submit_feedback(
        &self,
        search_id: &str,
        rating: i32,
        comment: &str,
    ) -> anyhow::Result<()> {
        self.api.feedback(search_id, rating, comment).await
    }

    async fn run_merge(&self) -> anyhow::Result<()> {
        self.api.merge().await
    }
}
```

`crates/app/src/lib.rs` に追加:

```rust
pub mod knowledge;
```

- [ ] **Step 4: テスト pass**

```bash
cargo test -p auto-trader record_trade_open_emits_ingest_with_correct_metadata
```

Expected: PASS。

- [ ] **Step 5: フル統合テスト + 静的解析**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。

- [ ] **Step 6: Commit**

```bash
git add crates/app/src/knowledge.rs crates/app/src/lib.rs
git commit -m "feat(app): add VegapunkKnowledgeStore (high-level facade)"
```

---

## Task 6: `swing_llm` を `Arc<dyn KnowledgeStore>` に移行

**Files:**
- Modify: `crates/strategy/src/swing_llm.rs` (struct名は `SwingLLMv1`)
- Modify: `crates/strategy/Cargo.toml` (auto-trader-core が既存なら追加不要)
- Modify: `crates/integration-tests/tests/phase3_swing_llm.rs`
- Delete: `crates/integration-tests/src/mocks/vegapunk_grpc.rs` (trait 化により tonic 偽サーバが不要に)
- Modify: `crates/integration-tests/src/mocks/mod.rs` (`pub mod vegapunk_grpc;` 行削除)

- [ ] **Step 1: 移行先テストを書く (phase3_swing_llm)**

現状の `create_strategy(&gemini.url(), &vegapunk.endpoint())` (Vegapunk は `MockVegapunkGrpc` tonic 偽サーバを起動して `VegapunkClient::connect` していた) を、`MockVegapunkApi` を `VegapunkKnowledgeStore` で包んで直接渡す形に書き換える。

`crates/integration-tests/tests/phase3_swing_llm.rs` の import と `create_strategy` を差し替え:

```rust
use auto_trader::knowledge::VegapunkKnowledgeStore;
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_integration_tests::mocks::vegapunk::{MockVegapunkApiBuilder, SearchResult};
use std::sync::Arc;
// 旧: use auto_trader_integration_tests::mocks::vegapunk_grpc::MockVegapunkGrpc;
// 旧: use auto_trader_vegapunk::client::VegapunkClient;

async fn create_strategy(
    gemini_url: &str,
    store: Arc<dyn KnowledgeStore>,
) -> SwingLLMv1 {
    SwingLLMv1::new(
        "swing_llm_v1".to_string(),
        vec![Pair::new(PAIR)],
        7, // holding_days_max
        store,
        gemini_url.to_string(),
        "test-api-key".to_string(),
        "gemini-2.0-flash".to_string(),
    )
}
```

各テストの `let vegapunk = MockVegapunkGrpc::start(vec![...]);` を以下に置換:

```rust
let mock_api = Arc::new(
    MockVegapunkApiBuilder::new()
        .with_search_results(vec![SearchResult { text: "USD/JPY bullish trend detected".to_string(), score: 0.9 }])
        .build()
);
let store: Arc<dyn KnowledgeStore> = Arc::new(VegapunkKnowledgeStore::new(mock_api.clone()));
let mut strategy = create_strategy(&gemini.url(), store).await;
```

注: `VegapunkKnowledgeStore` は `auto-trader` (app crate) にあるので、integration-tests は app crate 依存が必要。`crates/integration-tests/Cargo.toml` を確認:

```bash
grep "auto-trader\b" crates/integration-tests/Cargo.toml
```

なければ `auto-trader = { path = "../app" }` を追加。

- [ ] **Step 2: テスト fail を確認**

```bash
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests phase3_swing_llm 2>&1 | head -30
```

Expected: コンパイルエラー (新シグネチャ未対応)。

- [ ] **Step 3: `SwingLLMv1` のフィールドと `new` を書き換える**

`crates/strategy/src/swing_llm.rs:1-8` の use と `:9-25` の struct 定義を以下に変更:

```rust
use auto_trader_core::event::PriceEvent;
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_core::strategy::{MacroUpdate, Strategy};
use auto_trader_core::types::{Direction, Pair, Signal};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
// 削除: use auto_trader_vegapunk::client::VegapunkClient;
// 削除: use tokio::sync::Mutex;

pub struct SwingLLMv1 {
    name: String,
    pairs: Vec<Pair>,
    holding_days_max: u32,
    knowledge: Arc<dyn KnowledgeStore>,   // 旧: vegapunk: Mutex<VegapunkClient>
    gemini_client: reqwest::Client,
    gemini_api_url: String,
    gemini_api_key: String,
    gemini_model: String,
    last_check: HashMap<String, chrono::DateTime<chrono::Utc>>,
    last_attempt: HashMap<String, chrono::DateTime<chrono::Utc>>,
    consecutive_failures: HashMap<String, u32>,
    check_interval: chrono::Duration,
    latest_macro: Option<String>,
}
```

`:27-56` の `new` を以下に変更:

```rust
impl SwingLLMv1 {
    pub fn new(
        name: String,
        pairs: Vec<Pair>,
        holding_days_max: u32,
        knowledge: Arc<dyn KnowledgeStore>,    // 旧: vegapunk: VegapunkClient
        gemini_api_url: String,
        gemini_api_key: String,
        gemini_model: String,
    ) -> Self {
        let gemini_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build Gemini HTTP client");
        Self {
            name,
            pairs,
            holding_days_max,
            knowledge,                          // 旧: vegapunk: Mutex::new(vegapunk)
            gemini_client,
            gemini_api_url,
            gemini_api_key,
            gemini_model,
            last_check: HashMap::new(),
            last_attempt: HashMap::new(),
            consecutive_failures: HashMap::new(),
            check_interval: chrono::Duration::hours(4),
            latest_macro: None,
        }
    }
    // 他メソッド (should_check 等) は変更なし
}
```

`query_vegapunk_and_llm` (line 78 付近) の body 先頭部分を以下に置換:

```rust
async fn query_vegapunk_and_llm(
    &self,
    pair: &Pair,
    current_price: Decimal,
) -> anyhow::Result<Option<(Direction, Decimal, Decimal, Decimal, f64)>> {
    let search_result = self.knowledge
        .search_similar_patterns(pair, current_price, 5)
        .await?;
    let context: Vec<String> = search_result.hits.into_iter().map(|h| h.text).collect();

    // 以下、既存の Gemini 呼び出しはそのまま (context.join("\n") を使う部分も維持)
    // ...
}
```

旧 `let mut vp = self.vegapunk.lock().await;` から `drop(vp);` までの 3 行 (swing_llm.rs:88, 89, 98 周辺) を削除。

`crates/strategy/Cargo.toml` から `auto-trader-vegapunk` 依存を削除 (もう使わない)。`auto-trader-core` は既存なのでそのまま。

- [ ] **Step 4: `startup.rs::register_strategies` を `Arc<dyn KnowledgeStore>` 受け取りに変更**

`crates/app/src/startup.rs:159-167` のシグネチャ:

```rust
pub async fn register_strategies(
    engine: &mut StrategyEngine,
    strategies: &[StrategyConfig],
    pool: &PgPool,
    knowledge: &Option<Arc<dyn auto_trader_core::knowledge::KnowledgeStore>>,  // 旧: vegapunk_base, vegapunk_schema
    gemini_config: Option<&GeminiConfig>,
)
```

`startup.rs:198-207` の `clone_from_channel` ブロックを削除し、`startup.rs:210-218` の `SwingLLMv1::new` 呼び出しを変更:

```rust
let store = match knowledge {
    Some(s) => s.clone(),
    None => {
        tracing::warn!("knowledge_store unavailable, skipping strategy: {}", sc.name);
        continue;
    }
};

engine.add_strategy(
    Box::new(auto_trader_strategy::swing_llm::SwingLLMv1::new(
        sc.name.clone(),
        pairs,
        holding_days_max,
        store,                              // 旧: vp_client
        gemini.api_url.clone(),
        gemini_api_key,
        gemini.model.clone(),
    )),
    sc.mode.clone(),
);
```

`crates/app/src/main.rs` 側の `register_strategies` 呼び出し箇所 (grep で特定) を新シグネチャに合わせる。`main.rs:226-` で `VegapunkClient::connect` 直後に `VegapunkKnowledgeStore` でラップした `Arc<dyn KnowledgeStore>` を生成し、それを渡す:

```rust
let vegapunk_auth_token = std::env::var("VEGAPUNK_AUTH_TOKEN").ok();
let knowledge_store: Option<std::sync::Arc<dyn auto_trader_core::knowledge::KnowledgeStore>> =
    match auto_trader_vegapunk::client::VegapunkClient::connect(
        &config.vegapunk.endpoint,
        &config.vegapunk.schema,
        vegapunk_auth_token.as_deref(),
    ).await {
        Ok(client) => {
            tracing::info!("vegapunk connected: {}", config.vegapunk.endpoint);
            Some(std::sync::Arc::new(
                auto_trader::knowledge::VegapunkKnowledgeStore::new(std::sync::Arc::new(client))
            ))
        }
        Err(e) => {
            tracing::warn!("vegapunk unavailable: {e}. Knowledge ingestion disabled.");
            None
        }
    };

// register_strategies(engine, &config.strategies, &pool, &knowledge_store, gemini_cfg.as_ref()).await;
```

`vegapunk_base` / `clone_from_channel` 連鎖はこの段階では `main.rs:625, 784, 1638` に残っているが、Task 8/9/10 で除去するので OK。`knowledge_store` を Task 8/9 のフックで使う準備として保持しておく。

- [ ] **Step 5: テスト pass を確認**

```bash
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests phase3_swing_llm
```

Expected: 全 phase3_swing_llm テストが PASS。

- [ ] **Step 6: フル統合テスト + 静的解析**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。

- [ ] **Step 7: 不要になった `mocks/vegapunk_grpc.rs` を削除**

```bash
git rm crates/integration-tests/src/mocks/vegapunk_grpc.rs
```

`crates/integration-tests/src/mocks/mod.rs` から `pub mod vegapunk_grpc;` 行を削除。
合わせて `crates/integration-tests/Cargo.toml` の `[build-dependencies]` / `[dependencies]` に `tonic-build` 等が `MockVegapunkGrpc` 専用で残っていれば削除 (他テストで使っていなければ)。

再度フル統合テストで影響確認:

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。

- [ ] **Step 8: Commit**

```bash
git add crates/strategy/ crates/integration-tests/ crates/app/src/main.rs
git commit -m "refactor(strategy): swing_llm uses Arc<dyn KnowledgeStore> (mutex removed)"
```

---

## Task 7: `weekly_batch` を `Arc<dyn KnowledgeStore>` に移行

**Files:**
- Modify: `crates/app/src/weekly_batch.rs`
- Modify: 既存 weekly_batch テスト (あれば)

- [ ] **Step 1: 移行先の expectation を確認**

```bash
grep -n "VegapunkClient\|client.search" crates/app/src/weekly_batch.rs | head
```

`weekly_batch.rs:46` 付近の関数シグネチャと `:306` の `client.search(&query, "hybrid", 5)` 呼び出しを把握。

- [ ] **Step 2: シグネチャと呼び出しを書き換え**

`crates/app/src/weekly_batch.rs:46`:

```rust
pub async fn run_weekly_evolution(
    // ...
    knowledge: Option<&Arc<dyn auto_trader_core::knowledge::KnowledgeStore>>,
    // ...
) -> anyhow::Result<...> {
```

`:296` (内部関数 `search_strategy_pattern` 等) を:

```rust
async fn search_strategy_pattern(
    strategy_name: &str,
    knowledge: Option<&Arc<dyn auto_trader_core::knowledge::KnowledgeStore>>,
) -> Option<Vec<auto_trader_core::knowledge::PatternHit>> {
    let store = knowledge?;
    match store.search_strategy_outcomes(strategy_name, 5).await {
        Ok(res) => Some(res.hits),
        Err(e) => {
            tracing::warn!("vegapunk search failed for strategy {}: {e}", strategy_name);
            None
        }
    }
}
```

`main.rs` 側の呼び出し箇所 (`grep -n "run_weekly_evolution\|weekly_batch::" crates/app/src/main.rs`) で、`Option<&Arc<Mutex<VegapunkClient>>>` を `Option<&Arc<dyn KnowledgeStore>>` に置換。

- [ ] **Step 3: 既存の weekly_batch 関連テスト (integration-tests 内) を確認**

```bash
grep -rn "run_weekly_evolution\|weekly_batch::" crates/integration-tests/tests/
```

該当テストがあれば、`Arc<dyn KnowledgeStore>` 注入に書き換える。

- [ ] **Step 4: フル統合テスト + 静的解析**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/weekly_batch.rs crates/app/src/main.rs crates/integration-tests/
git commit -m "refactor(app): weekly_batch uses Arc<dyn KnowledgeStore>"
```

---

## Task 8: `main.rs` トレード OPEN フックを移行

**Files:**
- Modify: `crates/app/src/main.rs` (line 1460 周辺)

- [ ] **Step 1: 該当箇所を Read で再確認**

```bash
grep -n "vegapunk ingest failed for trade open\|format_trade_open" crates/app/src/main.rs
```

- [ ] **Step 2: `Mutex<VegapunkClient>` を `Arc<dyn KnowledgeStore>` に置き換え**

`main.rs:1460-1474` の block 全体 (`let mut vp = vp.lock().await;` から `tracing::warn!("vegapunk ingest failed for trade open: {e}");` まで) を以下で置換:

```rust
if let Some(store) = &knowledge_store {
    let store = store.clone();
    let trade_clone = trade_clone.clone();
    let indicators_clone = indicators_clone.clone();
    let alloc_pct_dec = rust_decimal::Decimal::try_from(alloc_pct).ok();
    tokio::spawn(async move {
        if let Err(e) = store.record_trade_open(&trade_clone, &indicators_clone, alloc_pct_dec).await {
            tracing::warn!("knowledge_store record_trade_open failed: {e}");
        }
    });
}
```

注: `knowledge_store: Option<Arc<dyn KnowledgeStore>>` は起動時に 1 度だけ作って、トレード処理ループに `Arc::clone` で渡す形に整理 (Task 10 でさらに集約)。

- [ ] **Step 3: 動作確認 — phase3_execution_flow が引き続き green**

```bash
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests phase3_execution
```

Expected: PASS。

- [ ] **Step 4: フル統合テスト + 静的解析**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/main.rs
git commit -m "refactor(app): trade-open hook uses KnowledgeStore::record_trade_open"
```

---

## Task 9: `main.rs` トレード CLOSE + Feedback フックを移行

**Files:**
- Modify: `crates/app/src/main.rs` (line 1570-1626 周辺)

- [ ] **Step 1: 該当箇所を再確認**

```bash
grep -n "format_trade_close\|vegapunk feedback failed\|compute_feedback_rating" crates/app/src/main.rs
```

- [ ] **Step 2: ingest + feedback 両方を `KnowledgeStore` 経由に**

`main.rs:1582-1626` 付近の block を以下で置換:

```rust
if let Some(store) = &knowledge_store {
    let store = store.clone();
    let t = t.clone();
    let entry_ind = entry_ind.clone();
    let bal_init = (bal, init);
    let pool = close_pool.clone();
    tokio::spawn(async move {
        let ctx = auto_trader_core::knowledge::TradeCloseContext {
            entry_indicators: entry_ind.as_ref(),
            account_balance: bal_init.0,
            account_initial: bal_init.1,
        };
        if let Err(e) = store.record_trade_close(&t, &ctx).await {
            tracing::warn!("knowledge_store record_trade_close failed: {e}");
        }

        // Auto-feedback if this trade had a Vegapunk search attached
        let search_id: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT vegapunk_search_id FROM trades WHERE id = $1",
        )
        .bind(t.id)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None)
        .flatten();

        if let Some(sid) = search_id {
            let rating = crate::enriched_ingest::compute_feedback_rating(&t);
            let net_pnl = t.pnl_amount.unwrap_or_default() - t.fees;
            let regime = entry_ind
                .as_ref()
                .and_then(|i| i.get("regime"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let comment = format!("PnL: {net_pnl}, regime: {regime}");
            if let Err(e) = store.submit_feedback(&sid.to_string(), rating, &comment).await {
                tracing::warn!("knowledge_store submit_feedback failed: {e}");
            }
        }
    });
}
```

- [ ] **Step 3: phase3_close_flow が引き続き green**

```bash
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests phase3_close
```

Expected: PASS。

- [ ] **Step 4: フル統合テスト + 静的解析**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/main.rs
git commit -m "refactor(app): trade-close + feedback hooks use KnowledgeStore"
```

---

## Task 10: 旧 `VegapunkClient` の `&mut self` メソッド削除 + 起動時配線集約

**Files:**
- Modify: `crates/vegapunk-client/src/client.rs`
- Modify: `crates/app/src/main.rs` (line 226-, 625-, 784-, 1638- の配線)
- Modify: `crates/app/src/startup.rs` (関連シグネチャ)

- [ ] **Step 1: 残存参照の有無を確認**

```bash
grep -rn "VegapunkClient" crates/ 2>/dev/null | grep -v target | grep -v "impl VegapunkApi for VegapunkClient" | grep -v "VegapunkClient::connect\|VegapunkClient::clone_from_channel"
```

`&mut self` メソッド (`ingest_raw` / `search` / `feedback` / `merge`) を直接呼んでいる箇所が出力されたら、KnowledgeStore 経由に書き換える必要あり。

```bash
grep -rn "\.ingest_raw\|\.search(\|\.feedback(\|\.merge(" crates/ 2>/dev/null | grep -v target | grep -i vegapunk
```

- [ ] **Step 2: 旧 `&mut self` メソッドを削除**

`crates/vegapunk-client/src/client.rs:67-131` の 4 メソッド (`pub async fn ingest_raw` / `search` / `feedback` / `merge`) を削除。`connect` と `clone_from_channel` は残置。

- [ ] **Step 3: `main.rs` の vegapunk 配線を 1 箇所に集約**

起動 prelude 内 (`main.rs:226` 周辺) で:

```rust
let vegapunk_auth_token = std::env::var("VEGAPUNK_AUTH_TOKEN").ok();
let knowledge_store: Option<std::sync::Arc<dyn auto_trader_core::knowledge::KnowledgeStore>> =
    match auto_trader_vegapunk::client::VegapunkClient::connect(
        &config.vegapunk.endpoint,
        &config.vegapunk.schema,
        vegapunk_auth_token.as_deref(),
    ).await {
        Ok(client) => {
            tracing::info!("vegapunk connected: {}", config.vegapunk.endpoint);
            Some(std::sync::Arc::new(
                auto_trader::knowledge::VegapunkKnowledgeStore::new(std::sync::Arc::new(client))
            ))
        }
        Err(e) => {
            tracing::warn!("vegapunk unavailable: {e}. Knowledge ingestion disabled.");
            None
        }
    };
```

`startup.rs` 側の `vegapunk_base: &Option<VegapunkClient>` を `knowledge: Option<&Arc<dyn KnowledgeStore>>` に置換 (line 163 周辺)。strategy 構築ループ (line 198-) で `clone_from_channel` していた箇所は `Arc::clone(&knowledge)` に変わる。

`main.rs:625, 784, 1638` の `clone_from_channel` 連鎖 (executor / daily / weekly batch 用) は全て削除し、`Arc::clone(&knowledge_store)` を渡す形に。

- [ ] **Step 4: `clone_from_channel` が呼ばれていないことを確認**

```bash
grep -rn "clone_from_channel" crates/ 2>/dev/null | grep -v target
```

Expected: マッチなし (もしくはテスト fixture だけ)。マッチが残る場合は Step 3 漏れ。

- [ ] **Step 5: フル統合テスト + 静的解析**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0。**ここで失敗すれば移行漏れがある。**

- [ ] **Step 6: phase4 (実 Vegapunk スモーク) で疎通確認**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  VEGAPUNK_AUTH_TOKEN="${VEGAPUNK_AUTH_TOKEN}" \
  cargo test -p auto-trader-integration-tests phase4_external -- --include-ignored
```

Expected: token あれば PASS、なければ SKIPPED。

- [ ] **Step 7: Commit**

```bash
git add crates/vegapunk-client/src/client.rs crates/app/src/main.rs crates/app/src/startup.rs
git commit -m "refactor(vegapunk): drop legacy &mut self methods, consolidate startup wiring"
```

---

## Task 11: 最終検証 + cleanup + PR 作成

**Files:** なし (検証のみ)

- [ ] **Step 1: 残骸 grep**

```bash
grep -rn "Mutex<VegapunkClient>\|Mutex<auto_trader_vegapunk" crates/ 2>/dev/null | grep -v target
grep -rn "TODO(Phase 4)" crates/integration-tests/src/mocks/vegapunk.rs
```

Expected: 全て空 (もしくは無関係)。

- [ ] **Step 2: フル統合テスト (CLAUDE.md 必須コマンド)**

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests 2>&1 | tail -10
```

Expected: `test result: ok. N passed; 0 failed`。Task 0 で記録した N と同等以上。

- [ ] **Step 3: ワークスペース全体テスト + clippy + fmt**

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 全 pass、warning 0、fmt 差分なし。

- [ ] **Step 4: `simplify` スキル実行 (CLAUDE.md コミット前チェックリスト)**

`simplify` スキルを起動して全変更ファイルをレビュー。指摘があれば修正して再テスト。

- [ ] **Step 5: `code-review` スキル実行 (CLAUDE.md 厳守)**

`code-review` スキルを起動し、手順通り全工程実行。指摘対応 → 再テスト。

- [ ] **Step 6: PR 作成**

```bash
git push -u origin feat/vegapunk-knowledge-store
gh pr create --title "refactor: Vegapunk KnowledgeStore abstraction (ports & adapters)" --body "$(cat <<'EOF'
## Summary
- Introduce `core::vegapunk_port::VegapunkApi` (low-level seam, &self, domain return types)
- Introduce `core::knowledge::KnowledgeStore` (high-level domain facade)
- `VegapunkClient` impls `VegapunkApi`, `VegapunkKnowledgeStore` impls `KnowledgeStore`
- Consumer migration: swing_llm / weekly_batch / main.rs all use `Arc<dyn KnowledgeStore>`
- `Mutex<VegapunkClient>` removed; tonic channel multiplexing now utilized
- `clone_from_channel` wiring (4 sites in main.rs) collapsed to single startup factory
- `MockVegapunk` → `MockVegapunkApi` (impls `VegapunkApi`, usable in production paths)
- Spec: `docs/superpowers/specs/2026-05-11-vegapunk-knowledge-store-design.md`
- Plan: `docs/superpowers/plans/2026-05-11-vegapunk-knowledge-store.md`

## Test plan
- [x] `cargo test -p auto-trader-integration-tests` (smoke + phase1/2/3/4 全件 green)
- [x] `cargo test --workspace`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo fmt --all -- --check`
- [x] phase4_external が実 Vegapunk (`vegapunk.local:6840`) と疎通する (auth token 設定時)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 7: CI 確認**

```bash
gh pr checks --watch
```

Expected: 全 check 成功。

---

## Spec Coverage Check

spec の各セクションが計画でカバーされているか:

| spec セクション | 対応タスク |
|---|---|
| 低レベル trait `VegapunkApi` | Task 1 |
| 高レベル trait `KnowledgeStore` | Task 2 |
| `VegapunkClient` 実装 | Task 3 (追加) / Task 10 (旧削除) |
| `VegapunkKnowledgeStore` 実装 | Task 5 |
| `MockVegapunkApi` 改名 + trait 実装 | Task 4 |
| `MockVegapunkGrpc` 廃止 (trait 化により tonic 偽サーバ不要) | Task 6 Step 7 |
| swing_llm 移行 | Task 6 |
| weekly_batch 移行 | Task 7 |
| main.rs open 移行 | Task 8 |
| main.rs close + feedback 移行 | Task 9 |
| 起動時配線集約 / `register_strategies` シグネチャ更新 | Task 6 Step 4 (準備) / Task 10 (確定) |
| Mutex 撤廃 | Task 6-10 |
| テスト戦略 (本物の VegapunkKnowledgeStore + MockVegapunkApi) | Task 5 (unit), Task 6 (integration) |
| 実装順序の段階的 green 維持 | Task 0 baseline + 各タスク末尾の必須テスト |
