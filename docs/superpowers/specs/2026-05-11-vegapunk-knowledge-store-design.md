# Vegapunk KnowledgeStore 抽象化 設計

- 作成日: 2026-05-11
- ステータス: brainstorming 完了、未実装
- 関連: `specs/vegapunk-integration.md`, `crates/integration-tests/src/mocks/vegapunk.rs:1` の TODO

## 背景

現在 Vegapunk gRPC クライアント (`crates/vegapunk-client::VegapunkClient`) は具象型として直接 consumer (`crates/strategy/src/swing_llm.rs`, `crates/app/src/weekly_batch.rs`, `crates/app/src/main.rs` のトレード open/close フック) に握られている。

- consumer は `Mutex<VegapunkClient>` で包んで `&mut self` メソッドを呼ぶ必要があり、search 中に他スレッドの ingest が serialize される
- 起動時の `clone_from_channel` 連鎖が `main.rs` に散らばっている (`main.rs:226`, `:625`, `:784`, `:1638`)
- production code パスに `MockVegapunk` を注入できない (具象型直結のため)。`crates/integration-tests/src/mocks/vegapunk.rs:1` に `TODO(Phase 4): Introduce a VegapunkApi trait` と記載されたまま
- 検索クエリ文字列 (`"{}の現在の市場状況...".to_string()`) が strategy 側に散らばっており、ingest の `enriched_ingest` モジュール集中化と非対称

本設計はこれらをポート＆アダプタ構造で整理する。

## ゴール

1. consumer が `Arc<dyn KnowledgeStore>` のみに依存する
2. production code パスに mock を注入できる
3. text 整形ロジック (`enriched_ingest::format_trade_open` 等) の呼び出しを `KnowledgeStore` 実装の内部に集約
4. `Mutex<VegapunkClient>` を撤廃し、tonic channel の多重化を活かす

## 非ゴール (この PR では扱わない)

- proto 定義の変更
- `enriched_ingest` の文言改修 (移動と呼び出し元入れ替えのみ、動作変更なし)
- `VegapunkClient::connect` シグネチャの変更
- リトライ・サーキットブレーカー導入 (別 PR)
- 別バックエンド (内製ベクトル検索等) への差し替え (trait seam だけ用意、実装はしない)

## アーキテクチャ

```
core                           ← 抽象トレイトのみ
  ├ trait VegapunkApi          低レベル: 4 RPC をドメイン型で公開、&self
  └ trait KnowledgeStore       高レベル: ドメイン操作 (record_trade_open 等)

vegapunk-client                ← 低レベル実装
  └ impl VegapunkApi for VegapunkClient
     (tonic stub を clone してから呼ぶ。consumer は &self のみ)

app::knowledge                 ← 高レベル実装
  └ struct VegapunkKnowledgeStore { api: Arc<dyn VegapunkApi> }
     impl KnowledgeStore
     (enriched_ingest を呼んで api.ingest_raw に流す)

integration-tests::mocks
  ├ MockVegapunkApi             現 MockVegapunk を rename + VegapunkApi 実装
  └ (KnowledgeStore は本物の VegapunkKnowledgeStore を Arc<MockVegapunkApi> で組む)
```

## 低レベル trait: `VegapunkApi`

配置: `crates/core/src/vegapunk_port.rs`

```rust
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub text: String,
    pub score: f32,
}

/// 検索結果全体。proto では `search_id` は SearchResponse レベル (検索 1 回に 1 つ) で、
/// 後の Feedback RPC のキーになる。
#[derive(Debug, Clone)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub search_id: String,
}

#[derive(Debug, Clone, Copy)]
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

### 設計判断

- **&self**: tonic stub の Clone は cheap (Arc 参照カウント)。実装側で `self.client.clone()` してから呼ぶことで `&self` を維持。consumer 側 `Mutex` 不要
- **戻り値**: 全 consumer が現状 `IngestRawResponse` を破棄しているので `()` に簡素化。検索は `SearchResults { hits, search_id }` を返す形に集約 (`search_id` は次回 Feedback RPC のキー、proto 上も SearchResponse レベルなので 1:1)
- **SearchMode 列挙化**: `"local"`/`"hybrid"` の文字列リテラル直書きを型で防ぐ
- **error type**: codebase の流儀に揃え `anyhow::Result`
- **Send + Sync**: `Arc<dyn VegapunkApi>` で共有可能にする

### 低レベル実装: `VegapunkClient`

```rust
#[async_trait]
impl VegapunkApi for VegapunkClient {
    async fn ingest_raw(&self, text: &str, source_type: &str, channel: &str, timestamp: &str) -> anyhow::Result<()> {
        let mut client = self.client.clone();
        client.ingest_raw(IngestRawRequest { /* ... */ }).await?;
        Ok(())
    }

    async fn search(&self, query: &str, mode: SearchMode, top_k: i32) -> anyhow::Result<SearchResults> {
        let mut client = self.client.clone();
        let resp = client.search(SearchRequest {
            text: query.to_string(),
            mode: Some(mode.as_str().to_string()),
            top_k: Some(top_k),
            schema: self.schema.clone(),
            /* ... */
        }).await?.into_inner();
        let hits = resp.results.into_iter().map(|r| SearchHit {
            text: r.text.unwrap_or_default(),
            score: r.score.unwrap_or(0.0),
        }).collect();
        Ok(SearchResults { hits, search_id: resp.search_id })
    }
    // feedback / merge も同様
}
```

- `connect` と `clone_from_channel` は inherent method のまま残置 (factory が複数 store を作る用)
- 既存の `&mut self` メソッドは削除 (新 trait に置換)

## 高レベル trait: `KnowledgeStore`

配置: `crates/core/src/knowledge.rs`

```rust
use crate::models::{Trade, Pair};
use crate::indicators::Indicators;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct PatternHit {
    pub text: String,
    pub score: f32,
}

/// 過去パターン検索結果。後の `submit_feedback(&search_id, ...)` の入力になる
/// `search_id` をセットで返す。
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

#[async_trait]
pub trait KnowledgeStore: Send + Sync {
    async fn record_trade_open(
        &self,
        trade: &Trade,
        indicators: &Indicators,
        alloc_pct: Option<f64>,
    ) -> anyhow::Result<()>;

    async fn record_trade_close(
        &self,
        trade: &Trade,
        exit_reason: &str,
        pnl_pips: Option<f64>,
    ) -> anyhow::Result<()>;

    async fn record_market_event(&self, event: &MarketEvent<'_>) -> anyhow::Result<()>;

    async fn search_similar_patterns(
        &self,
        pair: &Pair,
        current_price: rust_decimal::Decimal,
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

### 高レベル実装: `VegapunkKnowledgeStore`

配置: `crates/app/src/knowledge.rs` (`enriched_ingest` と同じクレートに置く)

```rust
pub struct VegapunkKnowledgeStore {
    api: Arc<dyn VegapunkApi>,
}

impl VegapunkKnowledgeStore {
    pub fn new(api: Arc<dyn VegapunkApi>) -> Self { Self { api } }
}

#[async_trait]
impl KnowledgeStore for VegapunkKnowledgeStore {
    async fn record_trade_open(&self, trade, indicators, alloc_pct) -> Result<()> {
        let text = enriched_ingest::format_trade_open(trade, indicators, alloc_pct);
        let channel = format!("{}-trades", trade.pair.0.to_lowercase());
        let timestamp = chrono::Utc::now().to_rfc3339();
        self.api.ingest_raw(&text, "trade_signal", &channel, &timestamp).await
    }

    async fn search_similar_patterns(&self, pair, current_price, top_k) -> Result<PatternSearchResults> {
        let query = format!("{}の現在の市場状況とトレード判断。価格: {}", pair.0, current_price);
        let res = self.api.search(&query, SearchMode::Local, top_k).await?;
        Ok(PatternSearchResults {
            hits: res.hits.into_iter().map(|h| PatternHit { text: h.text, score: h.score }).collect(),
            search_id: res.search_id,
        })
    }
    // 他メソッドも同様: enriched_ingest 呼び出し + 適切なクエリ整形 + api delegation
}
```

## 移行マッピング

| 現コード | 移行先 |
|---|---|
| `Mutex<VegapunkClient>` 直握り | `Arc<dyn KnowledgeStore>` 受け取り |
| `crates/strategy/src/swing_llm.rs:88` `vp.lock().await.search(...)` | `store.search_similar_patterns(pair, current_price, 5).await` |
| `crates/app/src/weekly_batch.rs:306` `client.search(...)` | `store.search_strategy_outcomes(strategy_name, 5).await` |
| `crates/app/src/main.rs:1470` `vp.ingest_raw(format_trade_open(...), ...)` | `store.record_trade_open(&trade, &indicators, Some(alloc_pct)).await` |
| `crates/app/src/main.rs:1596` `vp.ingest_raw(format_trade_close(...), ...)` | `store.record_trade_close(&trade, exit_reason, pnl_pips).await` |
| `main.rs` の `connect` + 4 箇所の `clone_from_channel` | 起動時に 1 度だけ `VegapunkClient::connect` → `Arc::new(VegapunkKnowledgeStore::new(Arc::new(client)))` → 全 consumer に `Arc::clone` で配布 |

### 副次効果

- `Mutex<VegapunkClient>` 撤廃により search 中の ingest serialization 解消 (tonic channel が内部で多重化)
- `main.rs` の vegapunk 関連配線が 1 箇所に集約 (現状 4 箇所に分散)

## テスト戦略

- 新規 `MockVegapunkApi` (現 `MockVegapunk` を rename + `VegapunkApi` 実装、`&self` のまま維持)
- 高レベル経路の統合テストは本物の `VegapunkKnowledgeStore` を `Arc<MockVegapunkApi>` で組む。text 整形含めて end-to-end 検証
- 既存テストの影響範囲:
  - `crates/integration-tests/tests/phase3_swing_llm.rs`: strategy が `Arc<dyn KnowledgeStore>` を受ける形に書き換え
  - `crates/integration-tests/tests/phase4_external.rs`: 実 Vegapunk 接続のスモークは `VegapunkClient` 直叩きのままで OK (低レベル API の疎通確認なので)
  - `crates/integration-tests/tests/phase3_jobs.rs:360-` の `enriched_ingest_format_trade_open` 単体テスト: 変更なし

## エラーハンドリング

- `KnowledgeStore` の全メソッドは `anyhow::Result<()>` または `anyhow::Result<Vec<PatternHit>>`
- consumer 側は現状通り `if let Err(e) = ... { tracing::warn!(...) }` パターンを維持。Vegapunk 障害でトレード本体が止まらない fail-safe (PR #76 と同じ方針) は KnowledgeStore 利用側で継続

## 実装順序の方針

1. `core` に traits とサポート型を追加 (既存に影響なし)
2. `vegapunk-client` に `VegapunkApi` 実装を追加、旧 `&mut self` メソッドはまだ残す
3. `app::knowledge` に `VegapunkKnowledgeStore` を追加
4. `MockVegapunkApi` を作成 (現 `MockVegapunk` と並存)
5. consumer を 1 つずつ移行: swing_llm → weekly_batch → main.rs トレード open → main.rs トレード close
6. 旧 `&mut self` メソッドと `MockVegapunk` を削除
7. `main.rs` の起動時配線を整理

各段階で `cargo test -p auto-trader-integration-tests` (`CLAUDE.md` 必須項目) を green に保つ。
