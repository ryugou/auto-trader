# FX 自動売買ツール 設計仕様

## 概要

OANDA API を使った FX 自動売買ツール。短期ルールベース戦略とスイング LLM アシスト戦略を並行してペーパートレードで検証し、効果があるものを本番に昇格する。判断根拠は Vegapunk（GraphRAG エンジン）に蓄積し、戦略の品質を継続的に改善する。

## 目的

1. OANDA API で短期自動取引を行い、利益を出す
2. 複数の戦略を並行してペーパートレードで検証し、効果があるものを本番に昇格する
3. Vegapunk に判断根拠を蓄積し、戦略の品質を継続的に改善する
4. ダッシュボードで損益推移・戦略別成績を把握する
5. 将来的にスイングトレードのシグナル通知を追加し、モッピー案件のブローカーでも活用する

## トレード方針

### 自動取引（短期・デイトレ）

- メインの収益源。OANDA API で全自動発注
- 1〜5分間隔で価格を監視し、条件合致で自動発注
- テクニカル指標ベースのルールロジック。LLM は判断に使わない

### スイングトレード

- モッピー案件のブローカー（API なし）で手動発注するためのシグナル通知
- ツールがシグナルを出し、発注は人間が行う
- macro-analyst + Vegapunk + LLM による判断
- 通知は本番口座での手動発注フェーズから実装。Phase 0 ではペーパートレードのみ

### 戦略管理

- 戦略はプラグイン設計。Strategy trait を実装すれば追加可能
- 複数戦略を同時に走らせ、ペーパートレードで比較検証
- 本番とペーパーを並行運用（本番で 1 戦略を回しつつ、別の戦略をペーパーで検証）
- どの戦略に寄せるかはデータで判断（短期: 1 週間、スイング: 1 ヶ月が目安）

### 通貨ペア

- 動的に追加・変更可能。戦略ごとに対象ペアを設定
- USD/JPY に限定しない。時間帯による流動性も考慮
- **表記の正規化**: 内部表現は OANDA API 形式（`USD_JPY`）で統一する。Vegapunk 投入時やダッシュボード表示時に必要に応じて変換する。DB にも `USD_JPY` 形式で保存する

## アーキテクチャ

### イベント駆動

コンポーネント間は tokio の mpsc channel で非同期メッセージングを行う。

```
market-monitor --[PriceEvent]--> strategy-engine
                                    |
                         +----------+----------+
                         |          |          |
                    Strategy A  Strategy B  Strategy C
                    (短期ルール) (スイングLLM) (検証中)
                         |          |          |
                         +----------+----------+
                                    |
                              [SignalEvent]
                                    |
                         +----------+----------+
                         |                     |
                    paper-trader          trade-executor
                    (ペーパー)             (OANDA API)
                         |                     |
                         +----------+----------+
                                    |
                              [TradeEvent]
                                    |
                         +----------+----------+
                         |          |          |
                    recorder    vegapunk     dashboard
                    (PostgreSQL) (gRPC)      (HTTP API)
```

### 単一バイナリ

market-monitor, strategy-engine, macro-analyst 等は内部のモジュール/タスク。別プロセスではない。tokio のタスクとして並行実行し、channel で通信する。

### デプロイ構成

- **FX ツール**: MacBook Pro M1 で docker-compose（auto-trader + PostgreSQL）
- **Vegapunk**: fuj11-agent-01（別マシン）
- **通信**: Tailscale 経由で gRPC 接続（plaintext、TLS なし。Tailscale が暗号化を担保）
- **確認事項**: Vegapunk の gRPC ポート（デフォルト 3000）が Tailscale ACL で許可されていること

## コンポーネント設計

### market-monitor

- OANDA API から対象通貨ペアの価格を 1〜5 分間隔で取得
- テクニカル指標を算出（RSI, 移動平均, サポート/レジスタンス等）
- PriceEvent を発行

### strategy-engine

- PriceEvent を各戦略に配信
- 各戦略が返す Signal を集約して SignalEvent を発行
- 戦略ごとに対象ペアとモード（paper/live/disabled）を管理

**シグナル競合ルール:**
- 1ペア1ポジ制約は**戦略別**に適用する。各戦略が独立した仮想口座を持つイメージ。これにより同一ペアで短期戦略とスイング戦略を同時に検証できる
- 既にポジションが開いているペア（同一戦略内）に対する新規エントリーシグナルは無視する
- 同一バーで同一戦略から相反するシグナル（long vs short）が出た場合、いずれも発行しない（矛盾は見送り）
- close シグナルと新規 open シグナルが同バーで出た場合、close を先に処理する（ポジションを閉じてから新規を判定）
- 同方向のシグナルが複数戦略から出た場合、各戦略が独立なのでそれぞれ発行する

### 戦略プラグイン

```rust
trait Strategy: Send + 'static {
    fn name(&self) -> &str;
    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal>;
    fn on_macro_update(&mut self, update: &MacroUpdate);
}
```

trait は async fn を使う（async-trait または Rust 1.75+ のネイティブ async trait）。短期ルールベース戦略は即座に返すだけだが、スイング LLM 戦略は内部で Vegapunk Search + LLM API を呼ぶため非同期が必須。strategy-engine は各戦略を独立した tokio タスクで駆動するため、一方の LLM 呼び出しが他の戦略をブロックすることはない。

初期実装:
- **trend_follow_v1**: 短期ルールベース（MA クロス + RSI フィルター等）。on_price は同期的に即返し
- **swing_llm_v1**: スイング LLM アシスト（Vegapunk 検索 + LLM 判断）。on_price 内で gRPC + LLM API を await

### executor

```rust
trait OrderExecutor {
    async fn execute(&self, signal: &Signal) -> Result<Trade>;
    async fn open_positions(&self) -> Result<Vec<Position>>;
    async fn close_position(&self, id: &str) -> Result<Trade>;
}
```

実装:
- **paper-trader**: 仮想的に発注・決済、残高管理
- **oanda-executor**: OANDA API で実発注（Phase 0 では未実装）

### macro-analyst

Phase 0 では最小構成:
- **経済指標カレンダー**: Forex Factory をスクレイピング（公式 API なし、HTML パース）。Phase 0 の必須実装
- **ニュース収集**: Exa Search API（既存キーあり、REST）。Phase 0 の必須実装
- **Trading Economics API**: 有料（$20/月〜）。Phase 0 ではスタブとし、将来的に追加を検討
- LLM で要約 -> Vegapunk に IngestRaw

後回し:
- 戦略パラメータの自動調整

### vegapunk-client

- gRPC クライアント（tonic）
- IngestRaw: トレード判断・決済結果・市場イベントの蓄積
- Search: 過去の類似パターン検索
- Feedback: 決済後の判断品質評価
- Merge: 日次/週次のコミュニティ検出

詳細は `specs/vegapunk-integration.md` を参照。

### recorder

- TradeEvent を購読して PostgreSQL に保存
- 価格データ（candle）の蓄積
- 日次サマリーの集計: トレードクローズ時に都度 daily_summary を upsert する（日次バッチではない）。ダッシュボードは常に最新の集計を参照できる
- **max_drawdown の計算**: daily_summary の max_drawdown はトレード単体からは正しく算出できない。日次の終わり（UTC 0:00）に、その日の全クローズ済みトレードから累積損益曲線を構築し、ピークからの最大下落幅を計算して更新する。日中のリアルタイム値は trades テーブルから都度算出する

### backtest

- price_candles テーブルから過去データを読み込み
- PriceEvent を時系列順に Strategy に流す
- 同じ Strategy trait を使うので、リアルタイムの戦略をそのままバックテスト可能
- 明らかにダメな戦略の足切りが目的。作り込みは不要
- **スイング戦略のバックテストは Phase 0 対象外**。on_macro_update に依存する戦略は過去マクロデータの再生が必要になるため、まずは短期ルールベース戦略のバックテストのみ対応する。スイング戦略の検証はリアルタイムのペーパートレードで行う

### dashboard

- axum で REST API を提供
- React + Recharts でフロントエンド（frontend-design スキルで実装）
- 読み取り専用。操作機能なし
- 表示内容:
  - 損益推移（日次/週次/累計）
  - 勝率・期待値・最大ドローダウン
  - 戦略別の成績比較
  - 通貨ペア別の成績
  - 時間帯別の勝率
  - トレード履歴
  - 保有中ポジション一覧

## Crate 構成

```
auto-trader/
  Cargo.toml                    # workspace 定義
  crates/
    core/                       # イベント型、trait、設定
    market/                     # OANDA クライアント、テクニカル指標
    strategy/                   # Strategy trait + 各戦略実装
    executor/                   # OrderExecutor trait + paper/oanda
    vegapunk-client/            # proto、gRPC クライアント
    macro-analyst/              # 経済指標・ニュース -> Vegapunk 蓄積
    backtest/                   # 過去データ再生、Strategy 検証
    db/                         # PostgreSQL、マイグレーション
    dashboard-api/              # axum REST API
    app/                        # main バイナリ、全体の組み立て
  dashboard-ui/                 # React + Recharts
  proto/
    graphrag.proto              # Vegapunk proto（コピー管理）
  config/
    default.toml
  schemas/
    fx-trading.yml              # Vegapunk 用スキーマ
  migrations/                   # SQLx マイグレーション
  specs/
  docker-compose.yml
  Dockerfile
```

## データモデル（PostgreSQL）

```sql
-- トレード履歴
CREATE TABLE trades (
    id UUID PRIMARY KEY,              -- Vegapunk IngestRaw 時にもこの ID を metadata に含め、DB と Vegapunk で同一トレードを紐付ける
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    direction TEXT NOT NULL,          -- long / short
    entry_price DECIMAL NOT NULL,
    exit_price DECIMAL,
    stop_loss DECIMAL,
    take_profit DECIMAL,
    entry_at TIMESTAMPTZ NOT NULL,
    exit_at TIMESTAMPTZ,
    pnl_pips DECIMAL,
    pnl_amount DECIMAL,
    exit_reason TEXT,                 -- tp_hit / sl_hit / manual / signal_reverse
    mode TEXT NOT NULL,               -- live / paper / backtest
    status TEXT NOT NULL,             -- open / closed
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 価格データ（バックテスト用にも使用）
CREATE TABLE price_candles (
    id BIGSERIAL PRIMARY KEY,
    pair TEXT NOT NULL,
    timeframe TEXT NOT NULL,          -- M1 / M5 / M15 / H1 / H4 / D
    open DECIMAL NOT NULL,
    high DECIMAL NOT NULL,
    low DECIMAL NOT NULL,
    close DECIMAL NOT NULL,
    volume INTEGER,
    timestamp TIMESTAMPTZ NOT NULL,
    UNIQUE (pair, timeframe, timestamp)
);

-- 戦略設定の履歴
CREATE TABLE strategy_configs (
    id UUID PRIMARY KEY,
    strategy_name TEXT NOT NULL,
    version TEXT NOT NULL,
    params JSONB NOT NULL,
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 日次サマリー（ダッシュボード用）
CREATE TABLE daily_summary (
    id BIGSERIAL PRIMARY KEY,
    date DATE NOT NULL,
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    mode TEXT NOT NULL,               -- live / paper / backtest
    trade_count INTEGER NOT NULL DEFAULT 0,
    win_count INTEGER NOT NULL DEFAULT 0,
    total_pnl DECIMAL NOT NULL DEFAULT 0,
    max_drawdown DECIMAL NOT NULL DEFAULT 0,
    UNIQUE (date, strategy_name, pair, mode)
);

-- マクロイベント
CREATE TABLE macro_events (
    id UUID PRIMARY KEY,
    summary TEXT NOT NULL,
    event_type TEXT NOT NULL,         -- economic_indicator / central_bank / geopolitical
    impact TEXT NOT NULL,             -- high / medium / low
    event_at TIMESTAMPTZ NOT NULL,
    source TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

## 設定ファイル

```toml
[oanda]
api_url = "https://api-fxpractice.oanda.com"
# api_key は環境変数 OANDA_API_KEY（1Password + direnv）

[vegapunk]
endpoint = "http://fuj11-agent-01:3000"
schema = "fx-trading"

[database]
url = "postgresql://auto-trader:***@db:5432/auto_trader"

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

[[strategies]]
name = "swing_llm_v1"
enabled = true
mode = "paper"
pairs = ["USD_JPY", "EUR_USD"]
params = { holding_days_max = 14 }
```

## Phase 0 スコープ

### 含む

- OANDA デモ API 接続（価格取得）
- ペーパートレード（短期ルールベース + スイング LLM アシスト）
- バックテスト（戦略の足切り用、最小構成）
- macro-analyst 最小構成（経済指標 + ニュース -> Vegapunk 蓄積）
- Vegapunk 連携（根拠蓄積・検索・フィードバック）
- PostgreSQL にトレード履歴・価格データを保存
- ダッシュボード（損益推移・戦略別成績・トレード履歴）
- docker-compose で MacBook Pro M1 にデプロイ

### 含まない

- OANDA 実発注（trade-executor の oanda 実装）
- Slack 通知（手動発注フェーズから）
- macro-analyst の戦略パラメータ自動調整
- モッピー条件の追跡（ツールのスコープ外）

## 運用フェーズ

```
Phase 0: デモ口座でペーパートレード検証（今ここ）
Phase 1: OANDA 本番口座で小額実取引（戦略の安定を確認後）
Phase 2: スイングトレードのシグナル通知追加、モッピー案件消化
Phase 3: 運転資金増額、戦略の継続改善
```
