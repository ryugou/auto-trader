# Phase 3 Integration Tests: Trade Flow

## Goal

Phase 3A の戦略シグナルテストを実装する。各戦略を直接インスタンス化し、CSV フィクスチャから構築した PriceEvent を on_price に流して、期待通りのシグナルが発火する（またはしない）ことを検証する。

## Architecture

```
crates/integration-tests/
  src/helpers/trade_flow.rs    # PriceEvent 構築ヘルパー
  tests/phase3_bb_mean_revert.rs
  tests/phase3_donchian_trend.rs
  tests/phase3_squeeze_momentum.rs
  fixtures/phase3/             # CSV フィクスチャ
```

DB は不要（戦略を直接テストする）。CSV からキャンドルを読み取り、PriceEvent に変換して戦略に流す。

## Phase 3A: Strategy Signal Tests

### BB Mean Revert V1 (M5, BB(20,2.5σ), RSI(14), ATR(14))

Entry条件:
- Long: close < BB lower AND RSI < 25 AND curr.low < prev.low
- Short: close > BB upper AND RSI > 75 AND curr.high > prev.high
- 最低履歴数: max(20, 15, 15) + 1 = 21 本

| Test | Fixture | Expected |
|------|---------|----------|
| Long entry | bb_long_entry.csv | Signal(Long) |
| Short entry | bb_short_entry.csv | Signal(Short) |
| No signal | bb_no_signal.csv | None |
| ATR zero | bb_atr_zero.csv | None |
| History insufficient | bb_history_insufficient.csv | None |

### Donchian Trend V1 (H1, 20bar channel, ATR baseline 20bar)

Entry条件:
- Long: close > 20bar high AND ATR > baseline_atr
- Short: close < 20bar low AND ATR > baseline_atr
- 最低履歴数: 20 + 20 + 14 + 1 = 55 本

| Test | Fixture | Expected |
|------|---------|----------|
| Long breakout | donchian_long_breakout.csv | Signal(Long) |
| Short breakout | donchian_short_breakout.csv | Signal(Short) |
| No signal | donchian_no_signal.csv | None |
| ATR zero | donchian_atr_zero.csv | None |
| History insufficient | (inline, 10 bars) | None |

### Squeeze Momentum V1 (H1, TTM Squeeze, momentum)

Entry条件:
- Squeeze: BB(20,2σ) inside KC(20,1.5×ATR) for 3+ bars
- Fire: BB exits KC
- Long: momentum > 0 AND rising
- Short: momentum < 0 AND falling
- 最低履歴数: max(20,20,15,22,6) + 2 = 24 本

| Test | Fixture | Expected |
|------|---------|----------|
| Long entry | squeeze_long_entry.csv | Signal(Long) |
| Short entry | squeeze_short_entry.csv | Signal(Short) |
| No signal | squeeze_no_signal.csv | None |
| ATR zero | squeeze_atr_zero.csv | None |
| History insufficient | (inline, 10 bars) | None |

## CSV Fixture Design

各 CSV は `timestamp,open,high,low,close,volume,bid,ask` 形式。

### 価格シーケンス設計方針

1. **安定期**: 20+ 本の狭いレンジで BB/KC/チャネルを安定化
2. **トリガー期**: 最後の 1-3 本で急変動を作り、条件をトリガー
3. **非発火**: 全期間レンジ内で推移

## Implementation Steps

1. [x] Plan file 作成
2. [x] `helpers/trade_flow.rs` — CSV→PriceEvent 変換ヘルパー
3. [x] BB Mean Revert fixtures + tests (6 tests)
4. [x] Donchian Trend fixtures + tests (6 tests)
5. [x] Squeeze Momentum fixtures + tests (6 tests)
6. [x] コンパイル & テスト実行確認 — 18 tests all pass
