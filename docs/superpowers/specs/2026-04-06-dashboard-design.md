# ダッシュボード設計仕様

> **⚠️ 一部廃止 (2026-04-07)**
> 本仕様内で言及している `crypto_trend_v1` 戦略および `crypto_real` /
> `crypto_100k` paper account は **削除済み** です（migration
> `20260407000006_cleanup_legacy_crypto_trend.sql` 参照）。本ドキュメントは
> ダッシュボード v1 設計時の歴史的記録として残しています。

## 概要

auto-trader の成績確認・口座管理用ダッシュボード。既存の axum API にエンドポイントを追加し、React + Recharts でフロントエンドを構築する。

## アーキテクチャ

- バックエンド: `crates/app/src/api.rs` にダッシュボード用エンドポイントを追加（別 crate にしない）
- フロントエンド: `dashboard-ui/`（React + Recharts + Vite）
- 本番配信: axum が `dashboard-ui/dist/` を `tower-http::ServeDir` で静的配信
- 開発: Vite dev server（HMR）+ axum API（CORS 許可）
- CI: GitHub Actions で `npm run build` → `cargo build` → `cargo test`
- ポート: API は 3001（既存）、Vite dev server は 5173（開発時のみ）

## API エンドポイント

### 既存（口座 CRUD）

| メソッド | パス | 説明 |
|---------|------|------|
| GET | /api/paper-accounts | 口座一覧 |
| POST | /api/paper-accounts | 口座作成 |
| GET | /api/paper-accounts/{id} | 口座詳細 |
| PUT | /api/paper-accounts/{id} | 口座更新 |
| DELETE | /api/paper-accounts/{id} | 口座削除 |

### ダッシュボード（読み取り）

| メソッド | パス | 説明 |
|---------|------|------|
| GET | /api/dashboard/summary | 全体 KPI（勝率、期待値、最大DD、総損益） |
| GET | /api/dashboard/pnl-history | 損益推移（日次/週次/累計） |
| GET | /api/dashboard/strategies | 戦略別成績 |
| GET | /api/dashboard/pairs | ペア別成績 |
| GET | /api/dashboard/hourly-winrate | 時間帯別勝率 |
| GET | /api/trades | トレード履歴（ページネーション） |
| GET | /api/positions | 保有中ポジション一覧 |

### 共通フィルタクエリ

全ダッシュボードエンドポイントに対応:

- `exchange` — `oanda` / `bitflyer_cfd`
- `paper_account_id` — UUID
- `strategy` — 戦略名
- `pair` — 通貨ペア名
- `from` / `to` — 日付範囲（RFC3339）

### レスポンス例

#### GET /api/dashboard/summary

```json
{
  "total_pnl": 12345.67,
  "net_pnl": 12285.67,
  "total_fees": 60.00,
  "trade_count": 42,
  "win_count": 25,
  "loss_count": 17,
  "win_rate": 0.595,
  "expected_value": 293.94,
  "max_drawdown": 5000.00,
  "profit_factor": 1.85
}
```

#### GET /api/dashboard/pnl-history

```json
{
  "period": "daily",
  "data": [
    { "date": "2026-04-01", "pnl": 500.00, "cumulative": 500.00 },
    { "date": "2026-04-02", "pnl": -200.00, "cumulative": 300.00 }
  ]
}
```

#### GET /api/dashboard/strategies

```json
[
  {
    "strategy": "crypto_trend_v1",
    "trade_count": 30,
    "win_rate": 0.60,
    "total_pnl": 8000.00,
    "max_drawdown": 3000.00
  }
]
```

#### GET /api/dashboard/hourly-winrate

```json
[
  { "hour": 0, "trade_count": 5, "win_count": 3, "win_rate": 0.60 },
  { "hour": 1, "trade_count": 3, "win_count": 1, "win_rate": 0.33 }
]
```

#### GET /api/trades

```json
{
  "trades": [
    {
      "id": "uuid",
      "strategy_name": "crypto_trend_v1",
      "pair": "FX_BTC_JPY",
      "exchange": "bitflyer_cfd",
      "direction": "long",
      "entry_price": "15000000",
      "exit_price": "15400000",
      "quantity": "0.01",
      "pnl_amount": "4000",
      "fees": "60",
      "net_pnl": "3940",
      "entry_at": "2026-04-05T10:00:00Z",
      "exit_at": "2026-04-05T14:00:00Z",
      "exit_reason": "tp_hit",
      "paper_account_id": "uuid",
      "status": "closed"
    }
  ],
  "total": 42,
  "page": 1,
  "per_page": 20
}
```

#### GET /api/positions

```json
[
  {
    "trade_id": "uuid",
    "strategy_name": "crypto_trend_v1",
    "pair": "FX_BTC_JPY",
    "exchange": "bitflyer_cfd",
    "direction": "long",
    "entry_price": "15000000",
    "quantity": "0.01",
    "stop_loss": "14800000",
    "take_profit": "15400000",
    "entry_at": "2026-04-05T10:00:00Z",
    "paper_account_id": "uuid",
    "paper_account_name": "crypto_real"
  }
]
```

## データソース

| 画面 | テーブル | 備考 |
|------|---------|------|
| KPI（summary） | daily_summary + trades | trade_count/win_count は daily_summary、profit_factor は trades から算出 |
| 損益推移 | daily_summary | paper_account_id で口座別に分離済み |
| 戦略別成績 | daily_summary | strategy_name で GROUP BY |
| ペア別成績 | daily_summary | pair で GROUP BY |
| 時間帯別勝率 | trades | EXTRACT(HOUR FROM entry_at) で集計 |
| トレード履歴 | trades | net_pnl = pnl_amount - fees |
| 保有ポジション | メモリ（PaperTrader） | api.rs から Arc<PaperTrader> を参照 |

### ポジション取得の実装

保有中ポジションはメモリ上の PaperTrader にしかないため、api.rs の router に `Vec<(String, Arc<PaperTrader>)>` を State として渡す。PaperTrader の `open_positions()` を呼んで返す。

## フロントエンド画面

### 1. 概要ページ（/）

- KPI カード: 総損益、勝率、期待値、最大DD、profit factor
- 損益推移チャート（Recharts AreaChart）— 日次/週次/累計の切り替え
- 口座別の複利比較チャート（crypto_real vs crypto_100k の残高推移）

### 2. トレード履歴（/trades）

- テーブル: ソート（日時、PnL）、フィルタ（戦略、ペア、口座）、ページネーション
- 各行: ペア、方向、エントリー/エグジット価格、数量、PnL、手数料、net_pnl、保有時間

### 3. 分析ページ（/analysis）

- 戦略別成績（BarChart）
- ペア別成績（BarChart）
- 時間帯別勝率（BarChart、24時間）

### 4. 口座管理（/accounts）

- 口座一覧テーブル: 名前、取引所、残高、レバレッジ、戦略
- 作成フォーム
- 編集（インライン or モーダル）
- 削除（確認ダイアログ付き）

### 5. ポジション（/positions）

- 保有中ポジション一覧テーブル
- リロードボタン（リアルタイム更新なし）

### グローバルフィルタ

ヘッダーに常駐:
- 取引所フィルタ: 全体 / FX / 暗号資産
- 口座フィルタ: 全口座 / 個別口座
- 期間フィルタ: 今日 / 1週間 / 1ヶ月 / 全期間

## 技術スタック

### フロントエンド（dashboard-ui/）

- React 19
- TypeScript
- Vite
- Recharts（チャート）
- TanStack Table（テーブル）
- TanStack Query（データフェッチ）
- Tailwind CSS

### バックエンド（既存 crates/app/）

- axum（既存）
- tower-http（ServeDir で静的ファイル配信、CORS）
- sqlx（既存）

## CI

GitHub Actions workflow:

1. `dashboard-ui/` で `npm ci && npm run build`
2. ビルド成果物を `dashboard-ui/dist/` に生成
3. `cargo build --workspace`
4. `cargo test --workspace`
5. `cargo clippy -- -D warnings`

## ディレクトリ構成

```
auto-trader/
  crates/app/src/
    api.rs              # paper_accounts CRUD + dashboard endpoints
    api/
      dashboard.rs      # ダッシュボード用ハンドラ
      trades.rs         # トレード履歴ハンドラ
      positions.rs      # ポジションハンドラ
  dashboard-ui/
    package.json
    vite.config.ts
    src/
      App.tsx
      pages/
        Overview.tsx
        Trades.tsx
        Analysis.tsx
        Accounts.tsx
        Positions.tsx
      components/
        KpiCards.tsx
        PnlChart.tsx
        StrategyChart.tsx
        TradeTable.tsx
        AccountForm.tsx
        GlobalFilters.tsx
      api/
        client.ts       # fetch wrapper
        types.ts         # API レスポンス型
```
