# Vegapunk 学習ループ + レジーム型戦略進化

- 作成日: 2026-04-10
- 対象: Rust backend + Vegapunk スキーマ + Postgres

## 目的

既存 3 戦略 (bb_mean_revert / donchian_trend / squeeze_momentum) のトレード結果をレジーム分類付きで Vegapunk に蓄積し、その知識を使って `donchian_trend_evolve_v1` の「今のレジームに対して最適なパラメータセット」を自動選択する仕組みを構築する。baseline (crypto_normal_v1) と evolve (crypto_evolve_v1) を同時並走して効果を比較する。

## 核心の考え方

「常に最適なパラメータ」は存在しない。ドンチャンブレイクアウトの成績はレジームに強く依存する:

| レジーム | 特性 | 有効なパラメータ傾向 |
|---|---|---|
| trend (トレンド) | 一方向に動き続ける | 短い entry_channel (攻め)、狭い SL |
| range (レンジ) | 方向感なく振れる | 長い entry_channel (安全)、広い SL |
| high_vol (高ボラ) | 大きく振れる | 広い SL (ATR 連動)、allocation 抑制 |
| event_window (イベント前後) | テクニカル無効化 | エントリー抑制 or スキップ |

Vegapunk の役割は **「今がどのレジームで、そのレジームではどのパラメータセットが過去に成功したか」** を evidence 付きで引くこと。パラメータの数値最適化は古典的統計 (Wilson Score) に任せ、Vegapunk は **レジーム → パラメータ選択** の判断と根拠追跡に使う。

## スコープ

### この PR に含む

- Vegapunk スキーマ `fx-trading` の拡張 (MarketRegime / ParamSet / ParamChange ノード + エッジ追加)
- 全 3 戦略の ingest 充実 (market context + レジーム分類 + indicator snapshot)
- ATR(14) を bitflyer の indicator 計算に追加
- trades テーブルに `entry_indicators JSONB` カラム追加 (migration)
- `donchian_trend_evolve_v1` 戦略 (パラメータを DB から読む版)
- `crypto_evolve_v1` 口座 (30k, leverage 2x) の seed migration
- 週次バッチ: DB 集計 + Vegapunk search + LLM (Gemini) でパラメータ提案 → DB 更新 → Vegapunk に param_change ingest → 通知
- 自動 feedback: trade close 時に search_id を引いて rating + comment を Vegapunk に返す
- Merge の定期実行 (日次) でコミュニティ検出を回す
- Wilson Score lower bound の自前実装 (統計的信頼度判定)

### この PR に含まない

- squeeze_momentum のトレード頻度改善 (別 PR)
- Shadow Action Mode (Vegapunk 側の機能成熟待ち)
- FX (OANDA) 対応 (本番口座作成待ち)
- ダッシュボード UI での比較画面追加 (既存の口座別表示で比較可能)

## アーキテクチャ

### データフロー

```
[既存3戦略] ──(毎トレード)──> Enriched Ingest ──> Vegapunk (fx-trading schema)
                                  │                        │
                                  │ entry_indicators       │ Merge (日次)
                                  │ を JSONB 保存            │ → Leiden クラスタ形成
                                  ↓                        │ → Node2Vec 構造学習
                              trades table                 ↓
                                  │               コミュニティ構造
                                  │                        │
                              [週次バッチ]                  │
                                  │                        │
                    ┌─────────────┼────────────────────────┘
                    ↓             ↓
              DB SQL 集計    Vegapunk search
              (勝率,PnL等)  (レジーム×パラメータの
                             過去パターン)
                    │             │
                    └──────┬──────┘
                           ↓
                    LLM (Gemini) に両方渡す
                           ↓
                    パラメータ調整案 (JSON)
                           ↓
              ┌────────────┼─────────────┐
              ↓            ↓             ↓
         DB 更新      Vegapunk に     通知ベルに
         (evolve      param_change   INSERT
          params)     として ingest
```

### 自動 feedback ループ

```
シグナル発火
  ↓
Vegapunk search: 「このレジーム × 条件の過去結果」
  ↓ search_id を取得
trades テーブルに search_id 保存 (新カラム vegapunk_search_id)
  ↓
PaperTrader 実行
  ↓ (数時間〜数日)
trade close
  ↓
recorder task:
  1. 通常の close 処理 (daily_summary, Vegapunk ingest)
  2. vegapunk_search_id があれば → 結果から rating 自動算出 → vp.feedback()
```

## Vegapunk スキーマ拡張

`schemas/fx-trading.yml` に追加:

### ノード変更

**MarketRegime (新規ノード):**

独立ノードにする。「市場の状態」は概念実体であり、`TradeDecision --OCCURRED_IN--> MarketRegime` で同レジーム下の全トレードが 1 ホップで集約される。Leiden コミュニティ形成に強く効く。

```yaml
nodes:
  MarketRegime:
    attributes:
      regime_type: { type: string, required: true }  # trend / range / high_vol / event_window
      period_start: { type: string }
      period_end: { type: string }
      atr_percentile: { type: string }
      adx_avg: { type: string }
```

**Strategy (既存ノード属性拡張):**

`ParamSet` を別ノードにせず、**Strategy ノードに param_set + 個別パラメータ属性を持たせる**。理由: Leiden で「同じ Strategy ノードに紐づく TradeDecision 群」がコミュニティとして浮きやすい。param_set 別に Strategy ノードが複数共存する形。

```yaml
nodes:
  Strategy:
    attributes:
      name: { type: string, required: true }         # donchian_trend_evolve_v1
      description: { type: string }
      version: { type: string }
      param_set: { type: string }                    # aggressive / normal / safe
      entry_channel: { type: string }                # "20"
      exit_channel: { type: string }                 # "10"
      sl_pct: { type: string }                       # "0.03"
      allocation_pct: { type: string }               # "1.00"
      atr_baseline_bars: { type: string }            # "50"
```

例: evolve のパラメータが週次で変わると、新しい Strategy ノードが生成される:
- `Strategy(name=donchian_trend_evolve_v1, param_set=week16, entry_channel=18, sl_pct=0.04)`
- `Strategy(name=donchian_trend_evolve_v1, param_set=week17, entry_channel=18, sl_pct=0.03)`

**MarketAnalysis (既存ノード属性追加):**

```yaml
nodes:
  MarketAnalysis:
    attributes:
      summary: { type: string, required: true }
      timeframe: { type: string }
      analysis_type: { type: string }
      regime_ref: { type: string }                   # MarketRegime への参照
      atr: { type: string }
      adx: { type: string }
```

**ParamChange (新規ノード):**

パラメータ変更履歴。変更の根拠が追跡できる。

```yaml
nodes:
  ParamChange:
    attributes:
      summary: { type: string, required: true }       # "SL_PCT 0.03→0.04, ENTRY_CHANNEL 20→18"
      rationale: { type: string, required: true }     # "RSI 70超ロングの3連敗を受けてSL拡大"
      changed_at: { type: string }
      week_label: { type: string }                    # "2026-W16"
```

### 追加エッジ

```yaml
edges:
  # ...既存エッジ...
  OCCURRED_IN: { from: TradeDecision, to: MarketRegime }     # そのトレードのレジーム
  CHANGED_FROM: { from: ParamChange, to: Strategy }          # 変更前の Strategy (パラメータ版)
  CHANGED_TO: { from: ParamChange, to: Strategy }            # 変更後の Strategy (パラメータ版)
  MOTIVATED_BY: { from: ParamChange, to: TradeResult }       # この結果がきっかけで変更した
```

### 追加 traceable_pairs

```yaml
traceable_pairs:
  # ...既存...
  - claim: ParamChange
    evidence: TradeResult
    edge: MOTIVATED_BY
  - claim: TradeDecision
    evidence: MarketRegime
    edge: OCCURRED_IN
```

## レジーム自動分類

candle データ + indicator から毎 PriceEvent ごとに regime を判定。判定ロジック:

```rust
fn classify_regime(indicators: &HashMap<String, Decimal>) -> MarketRegime {
    let adx = indicators.get("adx_14");
    let atr_pct = indicators.get("atr_percentile");  // 直近50本中の順位
    let bb_width = indicators.get("bb_width_pct");    // BB幅 / SMA比

    // Event window は外部データ (macro_analyst) が無いと判定できない。
    // 現状 macro_analyst は disabled なので、event_window は使わない。

    match (adx, atr_pct, bb_width) {
        // ADX > 25 = トレンドあり
        (Some(adx), _, _) if *adx > dec!(25) => MarketRegime::Trend,
        // ATR が上位 20% = 高ボラ
        (_, Some(pct), _) if *pct > dec!(80) => MarketRegime::HighVol,
        // それ以外 = レンジ
        _ => MarketRegime::Range,
    }
}
```

判定に必要な indicator (ADX, ATR percentile, BB width) を bitflyer / monitor の indicator 計算に追加する。

## 戦略: `donchian_trend_evolve_v1`

`donchian_trend_v1` のコピー。違い:

1. **パラメータを DB (paper_accounts テーブル or 新テーブル) から読む** — config/default.toml の const ではなく、runtime で可変
2. **シグナル発火前に Vegapunk search** — 「現在のレジームで、このパラメータセットの過去成績は？」を確認。Wilson Score lower bound が閾値未満なら skip
3. **search_id を trades テーブルに記録** — close 時の自動 feedback 用

### パラメータ保存先

新テーブル `strategy_params`:

```sql
CREATE TABLE strategy_params (
    strategy_name TEXT PRIMARY KEY REFERENCES strategies(name),
    params JSONB NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

evolve 戦略は起動時 + 週次更新時にこのテーブルからパラメータを読む。既存の const 版戦略は影響なし。

### 初期パラメータ

donchian_trend_v1 と完全同一:

```json
{
  "entry_channel": 20,
  "exit_channel": 10,
  "sl_pct": 0.03,
  "allocation_pct": 1.00,
  "atr_baseline_bars": 50
}
```

## 週次バッチ

`main.rs` の daily batch 近辺に weekly 判定を追加。日曜 0 時 (JST) に発火。

### 処理フロー

```
1. DB: 直近 1 週間の trades を戦略別集計
   SELECT strategy_name, COUNT(*), SUM(CASE WHEN pnl_amount > 0 THEN 1 END),
          AVG(pnl_amount), ...
   FROM trades WHERE exit_at > NOW() - INTERVAL '7 days'
   GROUP BY strategy_name

2. DB: 直近 1 週間の trades を regime 別集計
   SELECT entry_indicators->'regime' AS regime, COUNT(*), 勝率, 平均PnL
   FROM trades WHERE exit_at > NOW() - INTERVAL '7 days'
   GROUP BY regime

3. Wilson Score lower bound を各 regime × 戦略で計算
   → 統計的に弱い組み合わせを特定

4. Vegapunk search:
   query: "donchian_trend の損失パターン、レジーム別の傾向、直近のパラメータ変更結果"
   schema: "fx-trading"
   mode: "global"
   filter: { after: "1週間前", source_type: "trade_result" }

5. LLM (Gemini) に prompt:
   - 今週の DB 集計結果
   - Wilson Score 分析
   - Vegapunk search 結果 (過去パターン、パラメータ変更履歴)
   - 現行 evolve パラメータ
   → JSON でパラメータ調整案を返させる

6. strategy_params テーブル更新

7. Vegapunk に param_change として ingest:
   "donchian_trend_evolve_v1 パラメータ更新 (2026-W16)。
    ENTRY_CHANNEL 20→18, SL_PCT 0.03→0.04。
    根拠: range regime での SL hit 率 70%, Wilson lb 0.42。
    ATR baseline を短縮してトレード頻度向上を狙う。"

8. notifications テーブルに INSERT (ダッシュボードのベルに通知)
```

### LLM prompt 設計

```
あなたは自動売買戦略のパラメータチューナーです。

## 今週の成績 (DB)
{db_stats_json}

## レジーム別分析 (DB + Wilson Score)
{regime_analysis}

## Vegapunk からの過去パターン
{vegapunk_search_results}

## 現行パラメータ
{current_params_json}

## 目標
1. トレードの勝率を上げる (安全性)
2. トレード頻度を増やす (機会損失を減らす)

## 制約
- entry_channel: 10〜30 (整数)
- exit_channel: 5〜15 (整数)
- sl_pct: 0.01〜0.10
- allocation_pct: 0.50〜1.00
- atr_baseline_bars: 20〜100

## 出力
以下の JSON を返してください:
{
  "params": { "entry_channel": N, "exit_channel": N, "sl_pct": N, "allocation_pct": N, "atr_baseline_bars": N },
  "rationale": "変更理由を1-2文で",
  "expected_effect": "期待される効果を1文で"
}

変更不要と判断した場合は現行パラメータをそのまま返してください。
```

## 自動 feedback

### rating 算出ロジック

| 条件 | rating | comment テンプレート |
|---|---|---|
| TP hit / trailing exit + profit | 5 | "想定通りの成功。regime={r}, params={p}" |
| time_limit + profit | 4 | "利益だがトレンド弱。regime={r}" |
| time_limit + loss (微損) | 3 | "中立。方向感なし。regime={r}" |
| SL hit + 損失 < 口座の 3% | 2 | "小損。regime={r}, SL距離は適切か要検討" |
| SL hit + 損失 ≥ 口座の 3% | 1 | "大損。regime={r}, パラメータ見直し必要" |

### trades テーブル変更

```sql
ALTER TABLE trades ADD COLUMN entry_indicators JSONB;
ALTER TABLE trades ADD COLUMN vegapunk_search_id TEXT;
```

`entry_indicators` には OPEN 時の indicator snapshot + regime を保存:

```json
{
  "rsi_14": 72.3,
  "sma_20": 11420000,
  "sma_50": 11350000,
  "atr_14": 85000,
  "adx_14": 35.2,
  "atr_percentile": 65,
  "bb_width_pct": 3.2,
  "regime": "trend",
  "sma20_deviation_pct": 0.48
}
```

## Merge スケジュール

日次で `vp.merge(schema="fx-trading")` を実行。daily batch の一部として追加。コミュニティ検出 + Node2Vec 構造学習が走り、search の structural_weight 活用精度が上がる。

## Wilson Score 実装

```rust
/// Wilson Score lower bound for a binomial proportion.
/// p: observed win rate (0.0-1.0)
/// n: sample size
/// z: z-score for confidence level (1.96 for 95%)
fn wilson_lower_bound(wins: u64, total: u64, z: f64) -> f64 {
    if total == 0 { return 0.0; }
    let n = total as f64;
    let p = wins as f64 / n;
    let z2 = z * z;
    let numerator = p + z2 / (2.0 * n)
        - z * ((p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt());
    let denominator = 1.0 + z2 / n;
    (numerator / denominator).max(0.0)
}
```

evolve 戦略のシグナル発火前 validation:
- 現在の regime を判定
- DB から「この regime × 現行パラメータでの過去成績」を集計
- Wilson lower bound < 0.30 (勝率の 95% 下限が 30% 未満) → シグナル skip
- 閾値 0.30 は初期値。週次バッチで調整可能。

## 口座 seed

```sql
-- strategies catalog
INSERT INTO strategies (name, display_name, category, risk_level, description, algorithm, default_params)
VALUES ('donchian_trend_evolve_v1', 'ブレイクアウト進化版 (Donchian Evolve)',
        'crypto', 'medium',
        'donchian_trend_v1 ベース。Vegapunk 学習ループでパラメータを週次自動更新。baseline (normal) との比較検証用。',
        '(donchian_trend_v1 と同一アルゴリズム。パラメータのみ可変。)',
        '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb)
ON CONFLICT (name) DO NOTHING;

-- strategy_params (初期パラメータ)
INSERT INTO strategy_params (strategy_name, params)
VALUES ('donchian_trend_evolve_v1',
        '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb)
ON CONFLICT (strategy_name) DO NOTHING;

-- paper account
INSERT INTO paper_accounts (id, name, exchange, initial_balance, current_balance,
                            currency, leverage, strategy, account_type)
VALUES ('a0000000-0000-0000-0000-000000000020', 'crypto_evolve_v1',
        'bitflyer_cfd', 30000, 30000, 'JPY', 2,
        'donchian_trend_evolve_v1', 'paper')
ON CONFLICT (id) DO NOTHING;
```

## 既存コードへの影響

**変更:**
- `crates/market/src/bitflyer.rs` — indicator に ATR(14), ADX(14), ATR percentile, BB width 追加
- `crates/app/src/main.rs` — evolve 戦略登録、ingest テキスト拡張、regime 分類ロジック、週次バッチ、Merge 日次実行、feedback 自動送信
- `crates/executor/src/paper.rs` — entry_indicators を trade INSERT 時に保存 (execute_with_quantity に indicators 引数追加)
- `crates/vegapunk-client/src/client.rs` — 変更なし (既存 API で足りる)
- `schemas/fx-trading.yml` — MarketRegime / ParamSet / ParamChange ノード + エッジ追加
- `config/default.toml` — donchian_trend_evolve_v1 戦略エントリ追加

**新規:**
- `crates/strategy/src/donchian_trend_evolve.rs` — パラメータ可変版 donchian
- `crates/app/src/weekly_batch.rs` — 週次分析 + LLM パラメータ提案 + DB 更新
- `crates/app/src/regime.rs` — レジーム分類ロジック
- `crates/app/src/wilson.rs` — Wilson Score 計算
- `migrations/20260410000001_evolve_strategy_and_params.sql`

**変更なし:**
- 既存 3 戦略のコード (bb_mean_revert / donchian_trend / squeeze_momentum)
- フロントエンド (既存 UI で比較可能)
- 既存 3 口座のパラメータ

## エッジケース

- **Vegapunk 未接続時**: ingest / search / feedback は fire-and-forget + warn log。evolve 戦略は Vegapunk なしでも動く (Wilson Score validation は DB のみで実行可能)
- **Gemini API key 未設定**: 週次バッチの LLM 提案ステップをスキップ、パラメータ変更なしで継続。warn + notification
- **データ不足 (trades < 10)**: Wilson Score の信頼区間が広すぎて判定不能 → 「データ不足」として変更提案を控える。最低 10 trades 溜まるまでは初期パラメータで運用
- **全 regime でスキップされてトレードゼロ**: Wilson lb 閾値を段階的に下げる (0.30 → 0.20 → 0.10) or 閾値自体をデータ量に応じて動的調整
- **Merge 中の search**: Merge は非同期ジョブ。実行中も search は旧データで動作。完了後に最新構造が反映される
