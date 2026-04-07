# 暗号資産ペーパートレード 設計仕様

> **⚠️ 一部廃止 (2026-04-07)**
> 本仕様内で言及している `crypto_trend_v1` 戦略および `crypto_real` /
> `crypto_100k` paper account は **削除済み** です（migration
> `20260407000006_cleanup_legacy_crypto_trend.sql` 参照）。現在の検証は
> `bb_mean_revert_v1` / `donchian_trend_v1` / `squeeze_momentum_v1` の
> 3 戦略 × `crypto_safe_v1` / `crypto_normal_v1` / `crypto_aggressive_v1`
> 各 30,000 円口座で行っています。本ドキュメントは当初設計の歴史的記録
> として残しています。

## 概要

bitFlyer Crypto CFD（Lightning API）を使った暗号資産のペーパートレード機能。既存の FX 自動売買アーキテクチャに統合し、同じイベント駆動パイプライン上で BTC/JPY の売買シミュレーションを行う。価格データは bitFlyer Lightning の Public API（WebSocket + REST）から取得し、注文はペーパートレードで実行する。将来的に Private API での本番移行が可能な設計とする。

## 目的

1. bitFlyer Crypto CFD の実価格データを使い、BTC/JPY の自動売買アルゴリズムをリスクゼロで検証する
2. 複数の資金パターン（実額 / 10万円）で並行運用し、複利効果を比較する
3. Vegapunk にアルゴリズムの試行錯誤と結果を蓄積し、戦略を継続的に進化させる
4. FX と同じダッシュボードで暗号資産の成績を確認する

## 前提・制約

- bitFlyer Crypto CFD を使用。差金決済取引（実際のコインは動かない）
- ロング・ショート両対応。クリプトの下降トレンドが長期化しやすい特性を活かす
- レバレッジ最大2倍（設定で変更可能）
- オーバーナイト手数料: 建玉金額の 0.04%/日。ペーパートレードでもシミュレーションに反映する
- 24/365 常時稼働（FX は平日のみだが、暗号資産は土日も動く）
- Phase 0 では Public API のみ使用（認証不要）。Private API は本番移行時に追加

## 対象通貨ペア

BTC-CFD/JPY のみ（API 上の product_code = `FX_BTC_JPY`）。

bitFlyer Crypto CFD は旧 Lightning FX の後継サービスで、API は完全互換。product_code も `FX_BTC_JPY` をそのまま引き継いでいる。

将来的に ETH, XRP の CFD 対応が追加された場合は、対象ペアを拡張可能な設計とする。現物ペア（ETH_JPY 等）への拡張もアーキテクチャ上は可能だが、現時点ではスコープ外。

## アーキテクチャ

### FX との統合方針

既存の crate 構造にそのまま暗号資産を組み込む（別 crate / 別バイナリにはしない）。

- `core`: Exchange enum の追加、型の汎用化
- `market`: bitFlyer クライアントを OANDA と並列に追加、MarketDataProvider トレイトで抽象化
- `strategy`: 暗号資産向けパラメータのストラテジー実装を追加
- `executor`: PaperTrader を複数インスタンス対応、CFD のレバレッジ・オーバーナイト手数料対応
- `db`: exchange カラムの追加、paper_account_id の紐付け
- `app`: bitFlyer 用の市場監視タスクを追加

### イベントフロー

```
bitFlyer WebSocket ──[Ticker]──> bitflyer-monitor
                                      |
                                      |──[PriceEvent(exchange=BitflyerCfd)]
                                      |
                                      v
                                strategy-engine
                                      |
                           +----------+----------+
                           |                     |
                      crypto_trend_v1        (将来追加)
                           |
                     [SignalEvent]
                           |
                +----------+----------+
                |                     |
          paper-trader           paper-trader
          (5,233円)              (100,000円)
                |                     |
                +----------+----------+
                           |
                     [TradeEvent]
                           |
                +----------+----------+
                |          |          |
           recorder    vegapunk    dashboard
```

### strategy-engine のルーティング

PriceEvent の配信先は `pair` と `exchange` の両方でフィルタリングする。FX のペア名と暗号資産のペア名は衝突しないが（`USD_JPY` vs `FX_BTC_JPY`）、exchange フィールドでも明示的に絞ることで、将来ペア追加時の安全性を担保する。

## コンポーネント設計

### core crate の変更

#### Exchange enum の追加

```rust
pub enum Exchange {
    Oanda,
    BitflyerCfd,
}
```

PriceEvent, Trade, Candle に `exchange` フィールドを追加し、strategy-engine のルーティングと DB 保存に使用する。

#### Exchange 文字列のシリアライズ規約

| コンテキスト | Oanda | bitFlyer CFD |
|---|---|---|
| Rust enum | `Exchange::Oanda` | `Exchange::BitflyerCfd` |
| DB / 設定ファイル / Vegapunk metadata | `oanda` | `bitflyer_cfd` |

snake_case で統一する。serde の `#[serde(rename_all = "snake_case")]` で自動変換する。

#### pip サイズ計算の汎用化

現在の `trend_follow.rs` にある FX 固定の pip サイズ計算を、ペアごとの設定に移行する:

```toml
[pair_config.FX_BTC_JPY]
price_unit = 1            # 最小変動単位（円）
min_order_size = 0.001    # 最低注文数量（BTC）

[pair_config.USD_JPY]
price_unit = 0.001        # 最小変動単位（円）
min_order_size = 1        # 最低注文数量（通貨単位）
```

注: `price_unit` と `min_order_size` は実装前に bitFlyer Lightning の公式ドキュメントで最新値を確認すること。

#### volume フィールドの型変更

`Candle.volume` を `Option<i32>` から `Option<u64>` に変更。暗号資産の取引量が大きくなり得るため。

### market crate の変更

#### MarketDataProvider トレイトの導入

OANDA と bitFlyer を切り替え可能にする抽象化:

```rust
#[async_trait]
pub trait MarketDataProvider: Send + Sync {
    async fn get_candles(&self, pair: &Pair, timeframe: &str, count: u32) -> Result<Vec<Candle>>;
    async fn get_latest_price(&self, pair: &Pair) -> Result<Decimal>;
}
```

既存の OandaClient をこのトレイトの実装に変換し、bitFlyer クライアントも同じトレイトを実装する。

#### bitFlyer クライアントの追加

**WebSocket 接続（価格データ取得）:**

- 接続先: `wss://ws.lightstream.bitflyer.com/json-rpc`
- `lightning_ticker_FX_BTC_JPY` チャンネルを subscribe
- Ticker から best_bid / best_ask / ltp（最終取引価格）/ volume を取得
- 切断時は exponential backoff で自動再接続

注: WebSocket URL・チャンネル名・Ticker フィールドの意味（volume が累積か直近か等）は実装前に公式ドキュメントで確認すること。特に volume の定義はローソク足の出来高（V）の計算に直結する。

**REST API（補助）:**

- `GET /v1/getboard?product_code=FX_BTC_JPY`: 板情報の取得（スリッページシミュレーション用）
- Rate limit: IP 単位で約 500回/分。超過すると 1時間 10回/分に制限される

注: REST API パス・rate limit の正確な値は実装前に公式ドキュメントで確認すること。

**ローソク足の自前構築:**

bitFlyer API にはローソク足エンドポイントがないため、WebSocket の Ticker データから自前で OHLCV を構築する:

- Ticker の ltp（最終取引価格）を時系列で蓄積
- 指定された timeframe（M1, M5, H1 等）ごとに OHLCV を集計
- volume は Ticker 受信間の差分から算出（累積値の場合）。正確な定義は公式ドキュメント確認後に決定
- 完成したローソク足から PriceEvent を生成
- テクニカル指標（RSI, MA 等）の計算は既存の indicator モジュールを共用

### strategy crate の変更

#### crypto_trend_v1 の追加

`trend_follow_v1` と同じ MA クロス + RSI フィルターのロジックを持つが、暗号資産のボラティリティに合わせたパラメータを使用:

- MA 期間を短く設定（暗号資産のトレンド転換が速いため）
- RSI 閾値を広めに設定（暗号資産はオーバーシュートしやすいため）
- ロング・ショート両方のシグナルを生成
- 具体的なパラメータ値はペーパートレードで検証して決定

既存の `Strategy` トレイトをそのまま実装。

### executor crate の変更

#### ポジションサイジング

リスク額ベースを採用:

- 1トレードの最大損失 = 証拠金残高 × リスク率（設定可能、デフォルト 2%）
- 数量 = 最大損失額 ÷ SL までの価格距離
- 数量が最低注文単位未満の場合はトレードを見送る
- ポジション価値（数量 × 価格 ÷ レバレッジ）が証拠金残高を超える場合もトレードを見送る

```rust
pub struct PositionSizer {
    risk_rate: Decimal,        // デフォルト 0.02（2%）
    min_order_sizes: HashMap<Pair, Decimal>,  // ペアごとの最低注文単位
}
```

`leverage` は `PositionSizer` には持たない。常に `PaperAccount.leverage`（口座設定）から取得する。口座ごとにレバレッジが異なるケース（検証用に1倍と2倍を比較等）に対応するため。

#### PaperTrader の複数インスタンス対応

設定ファイルの `[[paper_accounts]]` ごとに独立した PaperTrader インスタンスを生成:

- 各インスタンスが独立した証拠金残高・ポジションを管理
- 同じシグナルが全インスタンスに配信される
- 複利運用: 利益は証拠金に加算、損失は証拠金から減算
- PnL 計算（ロング）: `pnl_amount = (exit_price - entry_price) * quantity`
- PnL 計算（ショート）: `pnl_amount = (entry_price - exit_price) * quantity`

#### オーバーナイト手数料のシミュレーション

- 建玉を翌日に持ち越した場合、建玉金額 × 0.04% を証拠金から差し引く
- 日次で UTC 0:00（JST 9:00）にチェックし、オープンポジションに対して適用
- 手数料は trades テーブルの `fees` フィールド（DECIMAL）に累積加算する。ポジションが複数日持ち越される場合は毎日加算
- 損益計算: `net_pnl = pnl_amount - fees`。ダッシュボードでは net_pnl を表示する

#### スリッページシミュレーション

板情報を参照して現実的な約定価格を計算:

- 注文数量に対して板の ask/bid を積み上げ、加重平均価格を算出
- 板が薄い場合は実際よりも不利な価格で約定させる

#### 将来の本番移行

`OrderExecutor` トレイトの `BitflyerExecutor` 実装を追加するだけ:

- Private API で注文送信（`POST /v1/me/sendchildorder`）
- 残高照会（`GET /v1/me/getbalance`）
- 認証: API Key + API Secret の HMAC 署名

#### 既知のリスク: 冪等性

複数 PaperTrader への同一シグナル配信は口座間では独立しているため、口座をまたいだ二重エントリーは発生しない。同一 PaperTrader 内での open_positions 確認と execute の間の TOCTOU（Time-of-check to time-of-use）は、既存 FX の PaperTrader と同様のリスクが残る。PaperTrader は単一タスクで順次処理するため実運用上は問題にならないが、将来の本番 executor では API 呼び出しの冪等性（ネットワークエラー時のリトライで二重注文にならないこと）を含めて設計課題として対応する。

## データモデル

### DB スキーマの変更

既存テーブルに `exchange` カラムを追加。マイグレーションファイルの制約名は既存の `migrations/*.sql` の実際の名前に合わせること。

注: 以下の SQL は説明用の擬似コード。実際のマイグレーションでは ★ コメントの順序（paper_accounts 作成 → trades / daily_summary 変更）で適用すること。

```sql
-- exchange カラムの追加
ALTER TABLE trades ADD COLUMN exchange TEXT NOT NULL DEFAULT 'oanda';
ALTER TABLE price_candles ADD COLUMN exchange TEXT NOT NULL DEFAULT 'oanda';
ALTER TABLE daily_summary ADD COLUMN exchange TEXT NOT NULL DEFAULT 'oanda';

-- ★ マイグレーション順序: paper_accounts を先に作成してから trades を変更すること
-- trades に数量・レバレッジ・手数料・paper_account_id を追加
-- quantity: FX の既存行は NULL（FX はロット単位で別管理のため）
ALTER TABLE trades ADD COLUMN quantity DECIMAL;
ALTER TABLE trades ADD COLUMN leverage DECIMAL NOT NULL DEFAULT 1;
ALTER TABLE trades ADD COLUMN fees DECIMAL NOT NULL DEFAULT 0;
ALTER TABLE trades ADD COLUMN paper_account_id UUID REFERENCES paper_accounts(id);

-- UNIQUE 制約の更新
-- 制約名は既存マイグレーションの実名に合わせて変更すること
ALTER TABLE price_candles DROP CONSTRAINT <既存の制約名>;
ALTER TABLE price_candles ADD CONSTRAINT price_candles_exchange_pair_timeframe_timestamp_key
    UNIQUE (exchange, pair, timeframe, timestamp);

-- daily_summary: paper_account_id は NULL 許容（FX は NULL）
-- 既存行のバックフィルは不要（FX は NULL のまま）
ALTER TABLE daily_summary ADD COLUMN paper_account_id UUID REFERENCES paper_accounts(id);

ALTER TABLE daily_summary DROP CONSTRAINT <既存の制約名>;
ALTER TABLE daily_summary ADD CONSTRAINT daily_summary_unique_key
    UNIQUE (date, strategy_name, pair, mode, exchange, paper_account_id);

-- FX（paper_account_id = NULL）用の部分ユニークインデックス
-- ★ 部分インデックスは列追加・旧 UNIQUE 削除・新 UNIQUE 追加の後に作成すること
CREATE UNIQUE INDEX daily_summary_fx_unique
    ON daily_summary (date, strategy_name, pair, mode, exchange)
    WHERE paper_account_id IS NULL;
```

### paper_accounts テーブルの追加

```sql
CREATE TABLE paper_accounts (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    exchange TEXT NOT NULL,
    initial_balance DECIMAL NOT NULL,
    current_balance DECIMAL NOT NULL,
    currency TEXT NOT NULL DEFAULT 'JPY',
    leverage DECIMAL NOT NULL DEFAULT 1,
    strategy TEXT NOT NULL DEFAULT '',
    -- 'paper' (検証用) / 'live' (本番。資金は実口座に紐づく)
    account_type TEXT NOT NULL DEFAULT 'paper',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

### account_type と評価額

- `account_type` は `'paper'` / `'live'` の 2 値。ラベル表示は「ペーパー」「通常」。
- 作成時のみ指定可能。`UpdatePaperAccount` では変更不可。
- `daily_summary.account_type` も保存し、概要ページを paper / live で分離表示できる。
- 評価額（`evaluated_balance`）は `current_balance + 含み損益` で算出。含み損益は `price_candles` の最新 close を使用。
- 残高推移チャート (`/api/dashboard/balance-history`) は `initial_balance + 累積実現損益` を日次再構築する。

### ダッシュボードの集計キー

`daily_summary` は `(date, strategy_name, pair, mode, exchange, paper_account_id)` で集計。paper_account_id を UNIQUE に含めることで、同一戦略・ペアでも口座別にサマリーが分離される。NULL UNIQUE の扱いと部分ユニークインデックスの詳細は上記「DB スキーマの変更」セクションを参照。

「crypto_real vs crypto_100k の複利推移比較」は daily_summary から直接クエリ可能。

## Vegapunk 連携

### スキーマ

既存の `fx-trading` スキーマに統合する。暗号資産のトレード判断も同じノード定義（TradeDecision, TradeResult, Strategy）で管理する。FX と暗号資産のストラテジーを横断検索できるほうが、アルゴリズム進化トラッキングの価値が高い。

スキーマ名のリネームは行わない（`fx-trading` のまま）。将来的に名前が気になれば別途対応する。

### 用途

暗号資産では **アルゴリズム進化トラッキングに特化** する。FX のようなマクロ分析（経済指標、ニュース）は対象外。

投入するデータ:

1. **トレード判断と結果**: FX と同じ（TradeDecision + TradeResult）
2. **ストラテジー進化の記録**: パラメータ変更の理由と結果

投入例:

```
IngestRawRequest:
  text: "crypto_trend_v1 のMA期間を(10,30)から(8,21)に変更。
         FX_BTC_JPYの直近1週間の勝率が40%→55%に改善。
         ボラが大きい暗号資産では短い期間のほうが追従性が良い。"
  metadata:
    source_type: "strategy_evolution"
    channel: "fx_btc_jpy-trades"
    exchange: "bitflyer_cfd"
    timestamp: "2026-04-10T21:00:00+09:00"
```

検索パターン:

```
SearchRequest:
  text: "暗号資産でMA期間を変更した過去の結果"
  mode: "local"
  schema: "fx-trading"
```

### ファンダメンタル分析

暗号資産向けの macro-analyst 連携はスコープ外。ノイズが多く体系化しにくいため、テクニカル指標ベースのアルゴリズムに集中する。

## 設定ファイル

```toml
# 既存
[oanda]
api_url = "https://api-fxpractice.oanda.com"

# 追加
[bitflyer]
ws_url = "wss://ws.lightstream.bitflyer.com/json-rpc"
api_url = "https://api.bitflyer.com"
# api_key / api_secret は将来の本番用。環境変数で管理（1Password + direnv）

[pairs]
fx = ["USD_JPY", "EUR_USD"]
crypto = ["FX_BTC_JPY"]

[pair_config.FX_BTC_JPY]
price_unit = 1
min_order_size = 0.001

[pair_config.USD_JPY]
price_unit = 0.001
min_order_size = 1        # 通貨単位（OANDA の実装に合わせて確認）

[pair_config.EUR_USD]
price_unit = 0.00001
min_order_size = 1        # 通貨単位（OANDA の実装に合わせて確認）

[position_sizing]
method = "risk_based"
risk_rate = 0.02          # 1トレードの最大損失 = 証拠金の2%

[[strategies]]
name = "crypto_trend_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY"]
params = { ma_short = 8, ma_long = 21, rsi_threshold = 75 }

[[paper_accounts]]
name = "crypto_real"
exchange = "bitflyer_cfd"
initial_balance = 5233    # 実際の bitFlyer 口座残高に合わせた検証用
leverage = 2
currency = "JPY"

[[paper_accounts]]
name = "crypto_100k"
exchange = "bitflyer_cfd"
initial_balance = 100000
leverage = 2
currency = "JPY"
```

注: `pair_config` の値は実装前に bitFlyer 公式ドキュメントで最新値を確認すること。

## ダッシュボード

既存の表示項目にフィルター機能を追加:

- フィルター: 全体 / FX / 暗号資産 / ペア別
- 資金パターン別の比較表示（crypto_real vs crypto_100k の複利推移グラフ）
- それ以外の表示項目（P&L、勝率、ドローダウン等）は既存のまま

データソースの役割分担:
- **日次チャート・サマリー表示**: `daily_summary` から取得（paper_account_id で口座別に分離済み）
- **トレード一覧・ドリルダウン**: `trades` テーブルから取得（個別トレードの詳細、fees 含む net_pnl）

## 既存コードへの影響

### 修正が必要な箇所

| ファイル | 変更内容 |
|---|---|
| `crates/core/src/types.rs` | Exchange enum 追加、PriceEvent/Trade/Candle に exchange フィールド追加、Trade に quantity/leverage/fees/paper_account_id 追加、volume を u64 に変更 |
| `crates/core/src/config.rs` | BitflyerConfig, PairConfig, PaperAccountConfig, PositionSizingConfig 追加 |
| `crates/market/src/monitor.rs` | MarketDataProvider トレイト導入、OandaClient をトレイト実装に変換 |
| `crates/strategy/src/trend_follow.rs` | pip サイズ計算を PairConfig から取得するよう変更 |
| `crates/executor/src/paper.rs` | PositionSizer 追加、PnL 計算を数量ベースに変更、複数インスタンス対応、オーバーナイト手数料 |
| `crates/db/` | exchange カラム追加のマイグレーション、クエリの更新、daily_summary の upsert を paper_account_id 対応に変更（crypto パスでは paper_account_id を必ず渡す。NULL での upsert は禁止） |
| `crates/app/src/main.rs` | bitFlyer 用の市場監視タスク追加、複数 PaperTrader のワイヤリング |

### 新規追加

| ファイル | 内容 |
|---|---|
| `crates/market/src/bitflyer.rs` | bitFlyer Lightning WebSocket / REST クライアント |
| `crates/market/src/candle_builder.rs` | Ticker データからローソク足を構築 |
| `crates/market/src/exchange.rs` | MarketDataProvider トレイト定義 |
| `crates/strategy/src/crypto_trend.rs` | 暗号資産向けトレンドフォロー戦略（ロング・ショート両対応） |

## スコープ外

- bitFlyer Private API での本番注文
- 暗号資産向けマクロ分析（ニュース、ファンダメンタル）
- ポジションサイジングの固定割合方式
- bitFlyer 以外の取引所対応
- 現物取引（Lightning 現物）
- ETH, XRP 等の CFD 対応（将来 bitFlyer が対応した場合に検討）
