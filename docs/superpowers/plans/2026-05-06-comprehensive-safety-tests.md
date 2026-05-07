# Comprehensive Safety Test Suite — Implementation Plan

> **目的**: 「全 pass = 適切なトレードが必ず行える」状態を成立させる。実機相当のダミーデータで pipeline 全段を駆動し、想定される全パターンを実際に確認する。

**Branch:** `feat/comprehensive-safety-tests` (PR #81 ベース)
**Spec:** seller の指示「完全に安心できる状態にするためのテスト」

## 現状ギャップ (PR #80 + PR #81 の後)

1. End-to-end pipeline (strategy → signal → trader → trade → close → balance) を 1 回も走らせない
2. trade.stop_loss / take_profit / exit_price / pnl_amount / leverage / fees の数値検証がほぼゼロ
3. SL ヒット時の維持率 = Y (新式の核心) を flow で検証していない
4. 多取引所 routing が e2e ではなく filter 関数の単体テストのみ
5. close 系で exit_price / pnl_amount / exit_reason の値が未検証
6. 残高境界 (min_lot 切り捨て / 残高不足拒否) が flow で未検証
7. smoke_test.rs が実は smoke していない (mock 動作確認のみ)
8. 戦略の exit ロジック → trader.close_position の連結が未検証

## 設計方針

### 共通 fixtures / helpers

- `crates/integration-tests/src/helpers/pipeline.rs` (新規):
  - `PipelineHarness` 構造体 — DB pool / strategy / trader / price_store / signal_rx / trade_rx を 1 つにまとめる
  - `PipelineHarness::new(pool, strategy_name, exchange, balance, leverage)` で全部 wire up
  - `feed_warmup_candles(&mut self, candles: &[Candle])` で warmup
  - `feed_trigger(&mut self, candle: Candle) -> Option<Signal>` で 1 candle 投入 + signal 取得
  - `execute_signal(&self, signal: Signal) -> Trade` で trader.execute
  - `monitor_for_exit(&mut self, exit_candle: Candle) -> Option<Exit>` で exit trigger
  - `close(&self, trade_id, reason) -> Trade` で trader.close_position
  - `assert_balance_after_open(&self, expected_locked: Decimal)`
  - `assert_balance_after_close(&self, expected_pnl: Decimal)`

- `crates/integration-tests/src/helpers/sizing_invariants.rs` (新規):
  - `assert_post_sl_margin_level_ge(trade, Y)` — 仮想 SL ヒット時の維持率が Y 以上であることを assert
  - `compute_expected_quantity(balance, lev, alloc, sl, y, price, min_lot) -> Decimal`
  - `compute_expected_sl_price(entry, direction, sl_pct) -> Decimal`
  - `compute_expected_pnl(entry, exit, qty, direction, leverage, fees) -> Decimal`

### 戦略 × 取引所 × 方向の組合せ網羅

5 戦略 × 2 取引所 × 2 方向 = 20 シナリオ。ただし実運用ペアは bb_mean_revert / donchian_trend が両方、donchian_evolve が bitflyer のみ、squeeze_momentum が両方、swing_llm が gmo_fx のみ。実際は 14-16 シナリオ。

各シナリオ 1 テスト関数で:
1. account seed (exchange-specific balance/leverage)
2. strategy 構築 + warmup candles 投入
3. trigger candle 投入 → signal 取得 (assert: direction, allocation_pct, stop_loss_pct, take_profit_pct, max_hold_until)
4. trader.execute(signal) → trade 取得 (assert: quantity, entry_price, stop_loss, take_profit, leverage, status, fees)
5. account_events 確認 (assert: margin_lock 額 = quantity × entry / leverage)
6. current_balance 確認 (assert: initial - margin_lock)
7. exit candle 投入 → close_position (assert: exit_price, pnl_amount, exit_reason)
8. account_events 確認 (assert: margin_release + trade_close PnL)
9. current_balance 確認 (assert: initial + pnl - fees)
10. **post-SL 維持率 invariant**: 仮に SL ヒットしていたら margin level >= Y を assert

### 残高境界 (新規ファイル `phase3_sizing_boundaries.rs`)

- `account_too_small_rejects_signal` — 残高 5,000 で execute → "account balance too small"
- `min_lot_truncation_realistic` — bitflyer_cfd 30k で min_lot=0.001 切り捨てが正しく適用される
- `lc_constraint_binds_at_extreme_sl` — gmo_fx lev=10 + SL=20% で max_alloc<<1.0 になることを検証
- `multiple_open_positions_share_balance` — 同 account で複数 open するとき後続の sizing が available_balance で行われる

### 維持率 invariant (新規ファイル `phase3_liquidation_safety.rs`)

- `post_sl_margin_level_at_y_for_each_exchange` — bitflyer (Y=0.5) / gmo_fx (Y=1.0) で各々、SL ヒット計算後の維持率 ≥ Y
- `pre_sl_drawdown_stays_above_y` — SL に達する前の任意の含み損 (SL の 50% / 80% / 99%) でも margin level >= Y
- `gap_through_sl_breaches_y` — SL を超えた含み損が出た場合、margin level < Y になる (= 仕様通り)

### Routing E2E (新規ファイル `phase3_routing_e2e.rs`)

- `signal_for_btc_routes_to_bitflyer_only` — `FX_BTC_JPY` signal を multiple trader (bitflyer + gmo_fx) に流して bitflyer のみが trade を作る
- `signal_for_usdjpy_routes_to_gmofx_only` — `USD_JPY` signal は gmo_fx のみで trade
- `unknown_pair_routes_nowhere` — `EUR_USD` signal はどの trader でも trade を作らない
- 上記 3 件は signal_tx → executor task → trader.execute の経路で実機相当に駆動

### close 値検証 (`phase3_close_flow.rs` 既存に enrichment)

各 `close_position(...)` 後に必ず以下を assert:
- `closed_trade.exit_price` (Long close = bid, Short close = ask)
- `closed_trade.pnl_amount` (= (exit - entry) × qty × ±1, signed by direction)
- `closed_trade.exit_reason`
- `closed_trade.status == TradeStatus::Closed`
- `current_balance` が pnl 反映済み

### exit logic 連結 (`phase3_strategy_exit_e2e.rs` 新規)

各戦略の StrategyExitReason → trader.close_position 経路:
- `bb_mean_revert_mean_reached_triggers_close` — strategy が MeanReached を出す → trader.close_position が走る → trade.exit_reason == StrategyMeanReached
- 同 squeeze_momentum / donchian_trend など各戦略の exit reason 別

### smoke_test 強化

`full_integration_smoke_test` を真の E2E にする:
- 既存: account seed + price_candles insert + mock 動作確認
- 追加: PipelineHarness で bb_mean_revert を 1 周回す (warmup → trigger → execute → close → assert balance/pnl)
- 追加: gmo_fx で donchian_trend も同様に 1 周

## ファイル構成

| ファイル | 種別 | 内容 |
|---|---|---|
| `crates/integration-tests/src/helpers/pipeline.rs` | 新規 | PipelineHarness |
| `crates/integration-tests/src/helpers/sizing_invariants.rs` | 新規 | invariant assert / 期待値計算 |
| `crates/integration-tests/src/helpers/mod.rs` | 修正 | 新 helper をパス再公開 |
| `crates/integration-tests/tests/phase3_pipeline_e2e.rs` | 新規 | 5 戦略 × 2 exchange × 2 方向の network of tests |
| `crates/integration-tests/tests/phase3_sizing_boundaries.rs` | 新規 | 残高境界 |
| `crates/integration-tests/tests/phase3_liquidation_safety.rs` | 新規 | 維持率 invariant |
| `crates/integration-tests/tests/phase3_routing_e2e.rs` | 新規 | routing E2E |
| `crates/integration-tests/tests/phase3_strategy_exit_e2e.rs` | 新規 | 戦略 exit → trader.close 連結 |
| `crates/integration-tests/tests/phase3_close_flow.rs` | 修正 | exit_price/pnl/exit_reason の assert 追加 |
| `crates/integration-tests/tests/phase3_execution_flow.rs` | 修正 | trade.stop_loss/take_profit/leverage の assert 追加 |
| `crates/integration-tests/tests/phase3_execution.rs` | 修正 | 同上 |
| `crates/integration-tests/tests/phase3_integrity.rs` | 修正 | balance / margin / pnl の数値 assert 追加 |
| `crates/integration-tests/tests/phase3_monitoring.rs` | 修正 | SL/TP price + entry/exit/pnl の assert 追加 |
| `crates/integration-tests/tests/smoke_test.rs` | 修正 | full_integration_smoke_test に E2E 1 周を追加 |

## 実装手順 (TDD、subagent dispatch)

各タスクを別 subagent に出す:

### Task 1: 共通 helper 整備
- `pipeline.rs` + `sizing_invariants.rs` + `mod.rs`
- helper 自体の単体テスト (期待値計算が正しいか)

### Task 2: 戦略別 pipeline E2E
- 5 戦略 × 2 exchange × 2 方向 = ~16 テスト
- 各テストで step 1〜10 を完遂

### Task 3: 残高境界
- 4 テスト (sizing_boundaries.rs)

### Task 4: 維持率 invariant
- 6 テスト (liquidation_safety.rs)

### Task 5: routing E2E
- 3 テスト (routing_e2e.rs)

### Task 6: 戦略 exit 連結
- 5 戦略 × 各 exit reason ≈ 10-15 テスト (strategy_exit_e2e.rs)

### Task 7: 既存 flow テストの assertion enrichment
- close_flow / execution_flow / execution / integrity / monitoring に追加 (≈ 30+ assert 追加)

### Task 8: smoke_test 強化
- full_integration_smoke_test 改造

### Task 9: 全体 verify + PR
- `cargo test -p auto-trader-integration-tests` 全 green
- `cargo clippy --workspace -- -D warnings` clean
- code-review skill 経由で codex / Copilot レビュー

## 期待カバレッジ

実装後の数値目標:
- 新規テスト関数 ≈ **50+ 件**
- assertion 数 ≈ **追加 200+ 件** (trade fields, balance, account_events, signal fields)
- 戦略 × 取引所 × 方向の網羅 = 全パターン
- 維持率 invariant = 各 exchange で flow 経由検証
- routing = signal_tx → trader 経路の e2e

## スコープ外 (将来別 PR)

- 実 broker API への live test (現状は mock のみ)
- 性能テスト (latency / throughput)
- マルチアカウント / 多 pair 同時並行のストレス
- Vegapunk 本番統合 (現状 mock)
