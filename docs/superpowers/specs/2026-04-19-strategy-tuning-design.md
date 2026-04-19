# 戦略チューニング設計書

- 作成日: 2026-04-19
- ステータス: 承認済み
- 根拠: `docs/strategy-performance-review-2026-04-19.md` の KPI 分析結果

## 背景

5 日間のペーパートレード実績で、全戦略が KPI 未達:
- 安全(BB): R:R 0.79(目標 1.0+)— 負け額が勝ち額を上回る
- 通常/攻め(Donchian/Squeeze): 勝率 25-27%(目標 30%+)かつ R:R 1.4-1.5(目標 3.0+)

外部調査(出典: docs 記載)により、原因は 3 点に集約:
1. フラット % SL がボラティリティに非連動
2. ポジションサイジングが口座 95-100%(業界標準 1-2% リスク)
3. Donchian/Squeeze を M5 で運用(日足設計のロジック、偽ブレイクアウト多発)

## スコープ

### In-scope
1. ATR ベース動的 SL(全戦略)
2. リスク連動ポジションサイジング(全戦略)
3. Donchian / Squeeze の時間足を M5 → 1H(BB は M5 維持)

### Out-of-scope
- 部分利確(検証要、今回見送り)
- exit ロジック変更(on_open_positions はそのまま)
- Signal / Trader / DB の型変更
- 新戦略の追加

## 変更 1: ATR ベース動的 SL

### 現状
各戦略が `SL_PCT` 定数でフラット % を指定:
- bb_mean_revert_v1: `SL_PCT = 0.02`
- donchian_trend_v1 / evolve: `SL_PCT = 0.03`
- squeeze_momentum_v1: `SL_PCT = 0.04`

### 変更後
`on_price` 内で ATR(14) を算出し、ATR × 倍率 / entry_price で `stop_loss_pct` を動的計算。

```
stop_loss_pct = min((ATR(14) × multiplier) / entry_price, cap)
```

| 戦略 | ATR 倍率 | 上限キャップ | 根拠 |
|------|---------|------------|------|
| bb_mean_revert_v1 | 1.5 | 0.03 (3%) | ミーンリバージョン = タイトな SL。1.5× ATR は「直近のノイズ幅のすぐ外」 |
| donchian_trend_v1 / evolve | 3.0 | 0.05 (5%) | トレンドフォロー = 広めの SL。3× ATR は Turtle 原典の推奨範囲 |
| squeeze_momentum_v1 | 2.5 | 0.05 (5%) | Squeeze 後のウィップソーに耐える幅。3.0 は広すぎ、2.0 はタイトすぎ |

ATR(14) は既に各戦略の `on_price` で計算済み(indicators として保持)。追加の指標計算は不要。

### 影響範囲
- `crates/strategy/src/bb_mean_revert.rs`: `SL_PCT` 定数削除、`on_price` 内で ATR ベース計算
- `crates/strategy/src/donchian_trend.rs`: 同上
- `crates/strategy/src/donchian_trend_evolve.rs`: 同上
- `crates/strategy/src/squeeze_momentum.rs`: 同上
- テスト: SL 値のアサーションを ATR ベースに更新

## 変更 2: リスク連動ポジションサイジング

### 現状
各戦略が `ALLOCATION_PCT` 定数で口座の固定割合を指定(0.95〜1.00)。

### 変更後
ATR SL と連動し、1 トレードあたりのリスクが口座残高の `TARGET_RISK_PCT` になるよう `allocation_pct` を動的計算。

```
allocation_pct = TARGET_RISK_PCT / stop_loss_pct
```

| 定数 | 値 | 説明 |
|------|---|------|
| TARGET_RISK_PCT | 0.02 (2%) | 全戦略共通。1 トレードで口座の最大 2% をリスクにさらす |
| ALLOCATION_CAP | 0.50 (50%) | allocation_pct の上限。ボラ極低時に全額投入を防ぐ |

例:
- ATR SL = 1.5%、target_risk = 2% → allocation = 2/1.5 = 133% → cap 50% に制限
- ATR SL = 4%、target_risk = 2% → allocation = 2/4 = 50%
- ATR SL = 5%、target_risk = 2% → allocation = 2/5 = 40%

### 影響範囲
- 各戦略の `ALLOCATION_PCT` 定数削除
- `on_price` 内で `stop_loss_pct` 算出後に `allocation_pct` を計算
- Signal の `allocation_pct` フィールドに動的値をセット(型は既に Decimal、変更不要)

## 変更 3: Donchian / Squeeze の時間足を 1H に

### 現状
`crates/app/src/main.rs`:
```rust
const CRYPTO_TIMEFRAME: &str = "M5";
```
全戦略が M5 で共通。

### 変更後
戦略の種別に応じて timeframe を分離:
- bb_mean_revert_v1: M5(スキャルピング的、M5 適合)
- donchian_trend_v1 / evolve / squeeze_momentum_v1: 1H(トレンドフォロー、M5 は偽ブレイクアウト多発)

### 実装方針
bitFlyer WebSocket は tick を受信して `CandleBuilder` で candle を構築する。現状は M5 candle のみ生成。

**方針 A(推奨)**: CandleBuilder を M5 + 1H 両方の candle を生成するよう拡張。M5 candle は BB 戦略に、1H candle は Donchian/Squeeze 戦略に流す。main.rs の `StrategyEngine::on_price` 呼び出し時に、戦略が期待する timeframe と PriceEvent の timeframe が一致する場合のみ処理。

**方針 B**: 別の CandleBuilder インスタンスを 1H 用に立てる。WebSocket tick を分岐。

方針 A が CandleBuilder への変更 1 箇所で済み、シンプル。

### 影響範囲
- `crates/market/src/candle_builder.rs`: 複数 timeframe の candle を生成できるよう拡張
- `crates/app/src/main.rs`: 戦略毎の timeframe 設定、PriceEvent routing
- warmup: 1H candle も DB から読み込む(既存の `get_candles` は timeframe パラメータを持つ)
- Strategy trait: `on_price` は PriceEvent を受ける。PriceEvent に timeframe 情報は含まれる(Candle.timeframe フィールド)。戦略側で自分の timeframe と一致しなければ無視する、または main.rs 側でフィルタ。

## テスト方針

- 各戦略の既存テスト: SL / allocation のアサーションを ATR ベースに更新
- 新テスト: ATR 値に応じて SL / allocation が動的に変わることを確認
- 1H timeframe: CandleBuilder が M5 + 1H 両方を正しく emit するテスト
- 統合: 既存の全テストが pass すること(機能変更だが壊さない)

## KPI 検証

改修後、最低 1 週間のペーパートレードで以下を計測:

| KPI | BB 目標 | Donchian/Squeeze 目標 |
|-----|--------|---------------------|
| 勝率 | 55%+ | 30%+ |
| R:R | 1.0+ | 3.0+ |
| 期待値 | 0 超 | 0 超 |

1 週間後に `docs/strategy-performance-review-*.md` を再作成し、改修前後を比較。
