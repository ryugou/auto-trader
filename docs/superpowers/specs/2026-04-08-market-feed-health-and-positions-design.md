# Market Feed Health Monitoring + Positions Tab Improvements

- 作成日: 2026-04-08
- 対象: Rust backend (auto-trader-app, market crate) + React dashboard

## 目的

自動トレーダーが依存している価格フィード（bitflyer WebSocket など）が停止すると、約定価格が古くなりトレード判断・SL/TP 執行・含み損益計算のすべてが成立しなくなる。現状はフィード停止が運用者に見えず、ダッシュボード上の数値だけ眺めていても異常に気づけない。

この PR では:

1. **価格フィード健康監視** — バックエンドで最新 tick の鮮度を追跡し、stale / missing を API で公開、ダッシュボード全ページの上部に常時警告バナーを表示する。**これが本 PR の主目的**
2. **含み損益の表示** — 上記 API 経由で取得した現在価格を使い、Positions ページに「含み損益」列を追加する（補助機能）
3. **Positions タブの並び替え** — Positions タブを Overview の直後に移動（運用視点で優先度の高いタブを左に寄せる）
4. **既存列名・数値表示の整理** — Positions の `SL` / `TP` を `損切りライン` / `利確ライン` に、TradeTable の `PnL` 列削除・`Net PnL` → `純損益`、両タブとも数値は整数表示

## スコープ

- 新規 Rust `PriceStore` と AppState 拡張
- `GET /api/market/prices`、`GET /api/health/market-feed` の 2 エンドポイント
- React 側の `MarketFeedHealthBanner` コンポーネント（全ページ共通）。ヘルス API 自体に到達できない場合も専用のエラーバナーを表示する (fail-silent 禁止)
- Positions 改修（タブ移動、含み損益列、純損益列、列名変更、整数化）
- Positions API (`PositionResponse`) に `fees` フィールドを追加して純損益計算の入力にする
- Positions ページで `/api/market/prices` 到達失敗時の警告バナー
- TradeTable 改修（PnL 列削除、純損益リネーム、整数化）
- 評価額の色分け: Accounts ページ + TradeTable per-account 見出しの両方で、`evaluated_balance` を `initial_balance` と比較して緑 / 赤 / 中立に色付け

## 非スコープ

- 価格フィード停止時の通知送信（既存 notifications テーブルへの INSERT）— 別 PR
- 価格フィード復旧検知による notification
- 価格フィード停止時にトレーダーの open/close 処理を自動停止する制御
- FX (OANDA) の再有効化
- `close_position` の `price_diff × leverage` ゴミ分岐の除去（現行コードパスに到達しないデッドコードなので放置）
- Overview / Analysis / Accounts / Strategies ページの変更

## 含み損益・純損益ルール

**単一ルール:**

```
含み損益 = (current_price - entry_price) × quantity     （LONG）
         = (entry_price - current_price) × quantity     （SHORT）

純損益   = 含み損益 - 累計 fees
```

- 小数点以下切り捨て（整数表示）
- crypto / FX 共通。両方とも trades テーブルの `quantity` カラムに数量が保存される前提（`PositionSizer::calculate_quantity` を通って `execute_with_quantity` で INSERT される現行トレードフローがこれを保証）
- `trades.quantity IS NULL` の行に遭遇したら（現状の実データには存在しないが防御的に）含み損益・純損益ともに `-` 表示
- `current_price` が取得できない行は含み損益・純損益ともに `-` 表示。運用者はバナー側で既にアラートを受けているので、この行の値を詳細に制御する意味はない
- `fees` は `trades.fees` カラムからそのまま読み出す（overnight_fee などが累計された値）。Positions API の `PositionResponse` に新規フィールドとして追加

## アーキテクチャ

### Rust 側: PriceStore

**ファイル:** `crates/app/src/price_store.rs` (新規)

```rust
use auto_trader_core::types::{Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct LatestTick {
    pub price: Decimal,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedKey {
    pub exchange: Exchange,
    pub pair: Pair,
}

/// In-memory store of the latest tick seen per (exchange, pair).
/// Written by the price_rx loop in main.rs, read by the /api/market
/// and /api/health handlers via AppState.
#[derive(Debug, Default)]
pub struct PriceStore {
    latest: RwLock<HashMap<FeedKey, LatestTick>>,
    /// Feeds the operator expects to be up, derived from config at
    /// startup. Used by the health endpoint to distinguish
    /// "intentionally disabled" from "expected but missing".
    expected: Vec<FeedKey>,
}

impl PriceStore {
    pub fn new(expected: Vec<FeedKey>) -> Arc<Self> { ... }
    pub async fn update(&self, exchange: Exchange, pair: Pair, price: Decimal, ts: DateTime<Utc>);
    pub async fn get(&self, exchange: Exchange, pair: &Pair) -> Option<LatestTick>;
    pub async fn snapshot(&self) -> Vec<(FeedKey, LatestTick)>;
    pub fn expected(&self) -> &[FeedKey];
}
```

### Rust 側: main.rs 配線

- 起動時、設定 (`config`) から実際に動かす (exchange, pair) の組を列挙して `PriceStore::new(expected)` を作成
  - FX モニター有効 → FX pairs を push
  - bitflyer モニター → 設定の crypto pairs を push
- `AppState { pool, price_store: Arc<PriceStore> }` に同梱
- 既存の `price_rx.recv()` ループ (`main.rs:660` 付近) に `price_store.update(...)` 呼び出しを 1 行追加。candle の `close` を最新 tick 価格として扱う
- 既存の executor forward / monitor forward との並列動作を壊さないよう、`update` は non-blocking (write lock は短時間)

### Rust 側: API エンドポイント

#### `GET /api/market/prices`

**ファイル:** `crates/app/src/api/market.rs` (新規)

レスポンス:
```json
{
  "prices": [
    {
      "exchange": "bitflyer_cfd",
      "pair": "FX_BTC_JPY",
      "price": "11474709",
      "ts": "2026-04-08T13:10:00.317Z"
    }
  ]
}
```

- PriceStore の snapshot を serialize するだけ
- クエリパラメータ無し
- ページング無し（現状監視対象は一桁）

#### `GET /api/health/market-feed`

**ファイル:** `crates/app/src/api/health.rs` (新規)

レスポンス:
```json
{
  "feeds": [
    {
      "exchange": "bitflyer_cfd",
      "pair": "FX_BTC_JPY",
      "status": "healthy",
      "last_tick_age_secs": 3
    },
    {
      "exchange": "oanda",
      "pair": "USD_JPY",
      "status": "stale",
      "last_tick_age_secs": 320
    }
  ]
}
```

- 期待リスト (`PriceStore::expected`) を全部列挙し、それぞれ:
  - 最新 tick 無し → `status: "missing"`, `last_tick_age_secs: null`
  - 最新 tick あり、`NOW - ts <= 60s` → `status: "healthy"`
  - 最新 tick あり、`NOW - ts > 60s` → `status: "stale"`
- **OANDA が起動時に disabled なら expected リストに含まれないので報告されない**（= 誤警告を出さない）
- 判定は handler 内で `Utc::now()` と比較、設定値は固定の 60 秒（将来的に config 化可能）

#### ルート登録

`crates/app/src/api/mod.rs`:
- `mod market;`, `mod health;` を追加
- `router()` に以下を追加（auth middleware 配下）:
  ```rust
  .route("/market/prices", get(market::prices))
  .route("/health/market-feed", get(health::market_feed))
  ```

### フロントエンド: MarketFeedHealthBanner

**ファイル:** `dashboard-ui/src/components/MarketFeedHealthBanner.tsx` (新規)

- `useQuery(['market-feed-health'])` で 15 秒ごとにポーリング
- `feeds.some(f => f.status !== 'healthy')` のとき赤バナーを描画、健全時は `null` を返す
- バナー文言例:
  - stale: `⚠️ 市場フィード異常: bitflyer_cfd / FX_BTC_JPY (最終 tick 5 分前)`
  - missing: `⚠️ 市場フィード異常: bitflyer_cfd / FX_BTC_JPY (tick 未受信)`
- 複数の異常は改行して列挙
- Tailwind: `bg-red-700 text-white px-4 py-2 text-sm font-semibold`、ヘッダー直下に配置
- クローズボタン無し（問題が続く限り表示し続ける）

`App.tsx` の `<header>` と `<main>` の間に `<MarketFeedHealthBanner />` を配置。

### フロントエンド: Positions 改修

**ファイル:** `dashboard-ui/src/pages/Positions.tsx`

- `useQuery(['market-prices'])` で現在価格を取得
- 既存のテーブルに 2 つの新列を追加:「**含み損益**」「**純損益**」
- 各行の計算:
  - 含み損益: prices から `(exchange, pair)` 一致を探し、`sign × (current - entry) × quantity`（LONG: sign=+1、SHORT: sign=-1）、整数切り捨て。無ければ `-`
  - 純損益: 含み損益 - `Number(position.fees)`。含み損益が `-` なら純損益も `-`
  - profit / loss / zero で緑 / 赤 / 灰
- 列名変更: `SL` → `損切りライン`、`TP` → `利確ライン`
- 数値表示は全カラムとも `Math.round(...).toLocaleString()` で整数化
- `/api/market/prices` が `isError` のときはテーブル上部に専用の警告ボックスを表示（ヘルスバナーとは別経路なので独立して surface）
- ヘッダーでのソート順変更なし

### フロントエンド: TradeTable 改修

**ファイル:** `dashboard-ui/src/components/TradeTable.tsx`

- `pnl_amount` カラムを buildColumns から削除
- `net_pnl` のヘッダー文字列を `'Net PnL'` → `'純損益'` に変更
- `fees` カラムの数値表示を `formatNum` から `Math.round(Number(...)).toLocaleString()` ベースの新 helper に変更（新 helper `formatInt` を追加）
- `entry_price` / `exit_price` / `quantity` も整数表示
- per-account 見出しの `評価額` を `evaluated_balance` と `initial_balance` の比較で色分け（up=緑、down=赤、equal=中立）

### フロントエンド: Accounts ページ改修

**ファイル:** `dashboard-ui/src/pages/Accounts.tsx`

- `評価額` 列を TradeTable 見出しと同じロジックで色分け（up=緑、down=赤、数値 NaN のときは中立）
- それ以外の列は変更なし

### バックエンド: Positions API

**ファイル:** `crates/app/src/api/positions.rs`

- `PositionResponse` に `fees: Decimal` フィールドを追加（`trade.fees` からそのまま読み出し）
- `dashboard-ui/src/api/types.ts` の `PositionResponse` も `fees: string` を追加

### フロントエンド: タブ並び替え

**ファイル:** `dashboard-ui/src/App.tsx`

`navItems` を以下の順に変更:
```ts
const navItems = [
  { to: '/', label: '概要' },
  { to: '/positions', label: 'ポジション' },
  { to: '/trades', label: 'トレード' },
  { to: '/analysis', label: '分析' },
  { to: '/accounts', label: '口座' },
  { to: '/strategies', label: '戦略' },
]
```

ナビ順以外のルート定義・コンポーネントツリーは不変。

## エッジケース

- **モニター起動直後**: まだ tick が来ていない → `missing` 扱いでバナー表示。起動 60 秒は許容するか？ → **許容しない**（運用上「60 秒待てば直る」状況はほぼ稀で、起動失敗と区別できる方が価値が高い）
- **OANDA 未設定**: 期待リストに含まれない → バナーに出ない
- **bitflyer は動いているが特定ペアの tick が来ない**: そのペアだけ stale。バナーにペア単位で列挙
- **`price_store` 書き込み競合**: `tokio::sync::RwLock` で複数 reader + 単一 writer、update は write lock、read は read lock。15 秒間隔の polling と 1 秒未満の tick 間隔なら contention 無視可能
- **Positions 含み損益で現在価格 = エントリー価格**: 差分 0、含み損益 0、`+0` と表示（緑扱い / 灰色表示のどちらか）→ **灰色** (`text-gray-400`) で表示
- **trades.quantity IS NULL の行**: 現行トレードフローでは発生しないが、防御的に `-` 表示
- **pair 名の大文字小文字**: PriceStore のキーは core の `Pair` 型そのままを使う（既存の比較規則に従う）

## テスト観点

### Rust

- `PriceStore::update` → `get` が最新を返すこと
- `PriceStore::snapshot` が挿入順序と独立に全エントリを返すこと
- `expected` リストが起動時の config から正しく構築されること（unit テスト可能な場所に切り出す）
- health handler のステータス判定ロジック（`healthy` / `stale` / `missing`）— age 境界 59秒 / 60秒 / 61秒 / None

### フロントエンド

（テストフレームワーク無しのため手動確認項目として）

- モニター正常稼働時: バナー非表示、Positions の含み損益が数値表示
- bitflyer を docker 停止で止めた場合: 60 秒後にバナー出現、Positions の含み損益が `-` に
- OANDA 無設定: バナーに OANDA 行が出ない
- Positions / Trades タブの列名とナビ順が仕様通り
- 小数点以下が表示されていない

## マイグレーション

無し（DB スキーマ変更無し）。

## 既存コードへの影響

- `crates/app/src/main.rs`: AppState 拡張、PriceStore 生成、price_rx ループに update 呼び出し追加、expected_feeds 構築
- `crates/app/src/api/mod.rs`: mod 追加 + AppState に price_store + 2 ルート
- `crates/app/src/api/market.rs`: 新規
- `crates/app/src/api/health.rs`: 新規
- `crates/app/src/api/positions.rs`: PositionResponse に `fees` フィールド追加
- `crates/app/src/price_store.rs`: 新規
- `dashboard-ui/src/App.tsx`: nav 並び替え + banner 配置
- `dashboard-ui/src/pages/Positions.tsx`: 含み損益 + 純損益列追加、列名変更、整数化、価格 API エラー時の警告ボックス
- `dashboard-ui/src/pages/Accounts.tsx`: 評価額列の色分け
- `dashboard-ui/src/components/TradeTable.tsx`: PnL 削除、純損益リネーム、整数化、per-account 見出しの評価額色分け
- `dashboard-ui/src/api/types.ts`: Market / Health 型追加、PositionResponse に `fees`
- `dashboard-ui/src/api/client.ts`: API 関数追加
- `dashboard-ui/src/components/MarketFeedHealthBanner.tsx`: 新規（ヘルス API 到達不可時の専用分岐あり）
- `crates/core/src/types.rs`: 変更なし
- `crates/executor/src/paper.rs`: 変更なし
- 他ページ (Overview, Analysis, Strategies): 影響なし

## 将来の拡張余地

- 価格フィード停止時に `notifications` テーブルへ `market_feed_stale` kind を INSERT して bell に流す
- 価格フィードが一定時間復旧しない場合に自動で open/close を停止する safeguard
- stale 閾値の config 化
- exchange レベルでのロールアップ表示（ペア個別ではなく "bitflyer_cfd 全体" で状態集約）
