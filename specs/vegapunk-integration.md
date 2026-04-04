# Vegapunk 連携仕様

## 概要

Vegapunk（根拠追跡型 GraphRAG エンジン）を FX 自動売買ツールの判断根拠の蓄積・検索・改善に使用する。

Vegapunk は**価格分析や予測はしない**。役割は以下の 4 つ:

1. トレード判断の根拠を構造化して蓄積する
2. 過去の類似判断パターンを自動検出する
3. 「なぜその判断をしたか」を後から辿れるようにする
4. 判断の品質をフィードバックループで継続的に改善する

## 接続方式

- gRPC クライアントとして接続
- proto 定義は Vegapunk リポジトリからコピー管理（サブモジュールにしない）
- proto ソース: `graphrag-engine/proto/graphrag.proto`
- Rust クライアントコードは `tonic-build` で生成

## 使用する RPC

| RPC | 用途 | 呼び出し頻度 |
|---|---|---|
| `IngestRaw` | トレード判断・決済結果・市場イベントの投入 | トレード判断時、決済時、イベント発生時 |
| `Search` | 過去の類似パターン検索 | トレード判断前 |
| `Feedback` | 検索結果・判断品質の評価 | 決済後 |
| `Merge` | コミュニティ検出・構造パターン学習 | 日次または週次 |

## スキーマ設計

スキーマ名: `fx-trading`

**Source of truth**: `schemas/fx-trading.yml`。以下の YAML ブロックは設計意図の説明用。実装時は `schemas/fx-trading.yml` を正とし、このドキュメントとの乖離が生じた場合はファイルが優先される。

### ノード定義

```yaml
nodes:
  TradeDecision:
    attributes:
      pair: { type: string, required: true }        # USD_JPY, EUR_USD（OANDA 形式で統一）
      direction: { type: string, required: true }    # long / short / close
      entry_price: { type: string }
      stop_loss: { type: string }
      take_profit: { type: string }
      confidence: { type: string }                   # high / medium / low
      decided_at: { type: string }

  MarketAnalysis:
    attributes:
      summary: { type: string, required: true }      # "ドル高トレンド継続、RSI 65で過熱感なし"
      timeframe: { type: string }                    # 1h / 4h / 1d
      analysis_type: { type: string }                # technical / fundamental / sentiment

  TradeResult:
    attributes:
      summary: { type: string, required: true }
      pnl_pips: { type: string }
      exit_reason: { type: string }                  # tp_hit / sl_hit / manual / signal_reverse
      holding_time: { type: string }

  MarketEvent:
    attributes:
      summary: { type: string, required: true }      # "米雇用統計 予想+18万 結果+25万"
      event_type: { type: string }                   # economic_indicator / central_bank / geopolitical
      impact: { type: string }                       # high / medium / low

  Strategy:
    attributes:
      name: { type: string, required: true }         # "トレンドフォロー_MA_cross"
      description: { type: string }
      version: { type: string }
```

### エッジ定義

```yaml
edges:
  BASED_ON: { from: TradeDecision, to: MarketAnalysis }       # 判断の根拠
  TRIGGERED_BY: { from: TradeDecision, to: MarketEvent }      # きっかけ
  RESULTED_IN: { from: TradeDecision, to: TradeResult }       # 結果
  USED_STRATEGY: { from: TradeDecision, to: Strategy }        # 使った戦略
  CONTRADICTS: { from: MarketAnalysis, to: MarketAnalysis }   # 矛盾する分析
  SUPERSEDES: { from: TradeDecision, to: TradeDecision }      # 判断の修正
```

### 根拠追跡ペア（traceable_pairs）

```yaml
traceable_pairs:
  - claim: TradeDecision
    evidence: MarketAnalysis
    edge: BASED_ON
  - claim: TradeDecision
    evidence: TradeResult
    edge: RESULTED_IN
```

## データ投入

以下の例は gRPC メッセージの内容を擬似的に示したもの。実際の呼び出しは tonic の `IngestRawRequest` メッセージを使用する。

### トレード ID の紐付け

DB の `trades.id`（UUID）を Vegapunk IngestRaw の metadata に含めることで、DB とナレッジグラフで同一トレードを横断的に辿れるようにする。具体的には metadata の `channel` に `{pair}-trades`、テキスト本文に `trade_id: {uuid}` を含める。

### タイミング 1: トレード判断時

```
IngestRawRequest:
  text: "USD_JPY ロング判断。trade_id: 550e8400-e29b-41d4-a716-446655440000。
         4h足でMA20/MA50ゴールデンクロス。RSI 58で余裕あり。
         直近の米雇用統計が強く、ドル高継続と判断。SL: 149.50, TP: 151.00。
         トレンドフォロー戦略v2を適用。"
  metadata:
    source_type: "trade_signal"
    channel: "usd_jpy-trades"
    timestamp: "2026-04-04T10:00:00+09:00"
```

### タイミング 2: 決済時

```
IngestRawRequest:
  text: "USD_JPY ロング決済。trade_id: 550e8400-e29b-41d4-a716-446655440000。
         TP到達 +150pips。保有時間 18h。
         判断通りドル高が進行。エントリー根拠のMA クロスは有効だった。"
  metadata:
    source_type: "trade_result"
    channel: "usd_jpy-trades"
    timestamp: "2026-04-05T04:00:00+09:00"
```

### タイミング 3: 市場イベント発生時

```
IngestRawRequest:
  text: "米FOMC議事要旨公開。タカ派姿勢維持。利下げ観測後退。
         ドル買い圧力強まる見通し。"
  metadata:
    source_type: "market_event"
    channel: "macro-events"
    timestamp: "2026-04-03T03:00:00+09:00"
```

## 検索パターン

### 判断前: 過去パターンの確認

```
SearchRequest:
  text: "ゴールデンクロスでロングした過去の結果"
  mode: "local"
  schema: "fx-trading"
```

### 戦略の振り返り

```
SearchRequest:
  text: "トレンドフォロー戦略の勝率と傾向"
  mode: "global"
  schema: "fx-trading"
```

### 損切り時の原因分析

```
SearchRequest:
  text: "SL到達した判断の共通点"
  mode: "local"
  schema: "fx-trading"
```

## フィードバック

決済後に判断の質を評価する:

```
FeedbackRequest:
  search_id: "<決済時の検索 ID>"
  rating: 4
  comment: "根拠は正しかったが、エントリータイミングが遅れて利幅が縮小した"
```

フィードバックの蓄積により Vegapunk のプロンプトが自動改善され、MarketAnalysis の抽出精度が向上する。

## Merge の実行

日次または週次で実行:

```
MergeRequest:
  schema: "fx-trading"
```

コミュニティ検出により「似たトレード判断のクラスタ」が自動形成され、Node2Vec で構造パターンが学習される。

## 禁止事項

- **価格の時系列データを投入しない** — Vegapunk はテキスト向け。OHLCV データは自動売買ツール側で保持する
- **リアルタイムのシグナル生成に使わない** — LLM 推論は非同期。ミリ秒単位の判断には不向き
- **Vegapunk の検索結果だけでトレード判断しない** — あくまで「過去の根拠と結果の参照」。最終判断はツール側のロジック

## metadata 命名規約

| フィールド | 値 | 例 |
|---|---|---|
| `source_type` | `trade_signal` / `trade_result` / `market_event` | `"trade_signal"` |
| `channel` | `{pair}-trades` / `macro-events`（pair は OANDA 形式） | `"usd_jpy-trades"` |
| `timestamp` | RFC3339 形式（JST） | `"2026-04-04T10:00:00+09:00"` |
