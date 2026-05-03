# Integration Test Suite Design

## Goal

auto-trader の全 API エンドポイント × 全操作 + 全トレードフロー（正常系・異常系）を 1 コマンドで検証できるテスト基盤を構築する。将来の自動修正パイプライン（vibepod 連携）の土台となる。

## Architecture

### 実行構造

```
crates/integration-tests/          新クレート
  src/
    lib.rs                         テストインフラ（DB起動、seed、モック管理）
    phase1/                        基盤検証
    phase2/                        API 全エンドポイント
    phase3/                        トレードフロー（固定モックデータ）
    phase4/                        外部 API 検証（実API）
    mocks/                         モック実装
    helpers/                       共通ユーティリティ
  fixtures/                        CSV/TOML フィクスチャ
```

### 実行コマンド

```bash
# Phase 1-3（モックのみ、外部依存なし）
cargo test -p auto-trader-integration-tests

# Phase 4（実 API、ネットワーク必要）
cargo test -p auto-trader-integration-tests --features external-api
```

Phase 1-3 は vibepod 内で完結。テスト開始時に PostgreSQL を起動し、migration 適用、フィクスチャ投入、テスト実行、終了後に破棄。前のフェーズが落ちたら後続は実行しない。

## Phase 1: 基盤検証

| テスト | 内容 |
|--------|------|
| 設定バリデーション（正常） | `config_valid.toml` のロード成功 |
| 設定バリデーション（不正 exchange） | `config_unknown_exchange.toml` → エラー |
| 設定バリデーション（pairs 空） | `config_missing_pairs.toml` → 適切な処理 |
| 設定バリデーション（不正戦略名） | `config_invalid_strategy.toml` → スキップ |
| 設定バリデーション（disabled 戦略） | `config_disabled_strategy.toml` → スキップ確認 |
| 環境変数オーバーライド | LIVE_DRY_RUN / OANDA_API_KEY 等の env → config 上書き |
| DB 接続 + migration | pool 作成 → migration 適用成功 |
| warmup（M5） | 200 本の M5 キャンドルロード → 戦略履歴バッファ確認 |
| warmup（H1） | 200 本の H1 キャンドルロード |
| warmup（ゼロ） | キャンドルなし → 戦略が履歴不足で待機 |
| 戦略登録（全 5 戦略） | 正常登録確認 |
| 戦略登録（disabled） | enabled=false → スキップ |
| 戦略登録（unknown 名） | 不明な戦略名 → スキップ + warning |
| 戦略登録（GEMINI_API_KEY 未設定） | swing_llm スキップ |
| 戦略登録（vegapunk 未接続） | swing_llm スキップ |
| 戦略登録（strategy_params SQL エラー） | donchian_evolve → fallback + warning |
| 通知 purge | 起動時 purge_old_read() が古い既読通知を削除 |

## Phase 2: API 全エンドポイント

### accounts

| 操作 | 正常系 | 異常系 |
|------|--------|--------|
| POST /api/trading-accounts | 作成 (paper/live × bitflyer_cfd/gmo_fx/oanda) | 不正 account_type / 重複名 / 不正 exchange / 不正 currency / 存在しない strategy / 同一 exchange で live 重複 / 残高不足 |
| GET /api/trading-accounts | 一覧（全口座 + evaluated_balance） | — |
| GET /api/trading-accounts/:id | 単体取得 | 存在しない UUID → 404 |
| PUT /api/trading-accounts/:id | 更新 | 不正フィールド |
| DELETE /api/trading-accounts/:id | 削除 | トレードあり → FK 違反 409 |

### trades

| 操作 | 正常系 | 異常系 |
|------|--------|--------|
| GET /api/trades | 一覧 + 全フィルタ (exchange/account/strategy/pair/status) + ページネーション + total count 正確性 | 不正フィルタ値 / page=0 |
| GET /api/trades/:id/events | トレードイベント | 存在しない UUID |

### positions

| 操作 | 正常系 | 異常系 |
|------|--------|--------|
| GET /api/positions | open ポジション一覧 | ポジションなし → 空配列 |

### strategies

| 操作 | 正常系 | 異常系 |
|------|--------|--------|
| GET /api/strategies | 一覧 + カテゴリフィルタ | — |
| GET /api/strategies/:name | 単体取得 | 存在しない名前 → 404 |

### dashboard

| 操作 | 正常系 | 異常系 |
|------|--------|--------|
| GET /api/dashboard/summary | 集計（全フィルタ） | データなし期間 |
| GET /api/dashboard/pnl-history | PnL 履歴 | 不正日付フォーマット |
| GET /api/dashboard/balance-history | 残高推移 | — |
| GET /api/dashboard/strategy-stats | 戦略別統計 | — |
| GET /api/dashboard/pair-stats | ペア別統計 | — |
| GET /api/dashboard/hourly-winrate | 時間帯別勝率 | — |

### notifications

| 操作 | 正常系 | 異常系 |
|------|--------|--------|
| GET /api/notifications | 一覧 + kind/日付フィルタ + ページネーション | 不正 kind / 不正日付 |
| PUT /api/notifications/:id/read | 既読マーク | 存在しない ID |

### health / market / auth

| 操作 | 正常系 | 異常系 |
|------|--------|--------|
| GET /api/health/market-feed | フィードステータス | フィード未起動 |
| GET /api/market/prices | 価格スナップショット | データなし |
| auth | 正常 Bearer トークン | トークンなし / 不正トークン / 不正フォーマット |

## Phase 3: トレードフロー（固定モックデータ）

全テストは BitflyerCfd と GmoFx の両 exchange でパラメタライズ実行。

### BB Mean Revert

| テスト | フィクスチャ | 検証内容 |
|--------|-------------|---------|
| Long エントリー | `bb_long_entry.csv` | BB 下限 + RSI < 25 → Signal(Long) |
| Short エントリー | `bb_short_entry.csv` | BB 上限 + RSI > 75 → Signal(Short) |
| Long midline エグジット | `bb_long_midline_exit.csv` | midline 到達 + 1R OK → 戦略エグジット |
| Short midline エグジット | `bb_short_midline_exit.csv` | 同 Short |
| Long midline 未到達 | `bb_long_midline_not_reached.csv` | エグジットなし |
| Short midline 未到達 | `bb_short_midline_not_reached.csv` | エグジットなし |
| 非発火 | `bb_no_signal.csv` | BB 内 + RSI 中間 → Signal なし |
| 1R 未到達 Long | `bb_1r_not_reached_long.csv` | midline 条件到達 + 1R 未到達 → 抑制 |
| 1R 未到達 Short | `bb_1r_not_reached_short.csv` | 同 Short |
| ATR ゼロ | `bb_atr_zero.csv` | ボラティリティゼロ → 非発火 |
| 履歴不足 | `bb_history_insufficient.csv` | 21 本未満 → 非発火 |
| fill bid/ask | `bb_fill_bidask.csv` | Long→ask, Short→bid 約定 |

### Donchian Trend

| テスト | フィクスチャ | 検証内容 |
|--------|-------------|---------|
| Long ブレイクアウト | `donchian_long_breakout.csv` | 20bar 高値ブレイク |
| Short ブレイクアウト | `donchian_short_breakout.csv` | 20bar 安値ブレイク |
| Long trailing エグジット | `donchian_long_trailing_exit.csv` | 10bar trailing ブレイク |
| Short trailing エグジット | `donchian_short_trailing_exit.csv` | 同 Short |
| Long trailing 未ブレイク | `donchian_long_trailing_no_break.csv` | チャネル内 → エグジットなし |
| Short trailing 未ブレイク | `donchian_short_trailing_no_break.csv` | 同 Short |
| 非発火 | `donchian_no_signal.csv` | チャネル内推移 |
| 1R 未到達 Long | `donchian_1r_not_reached_long.csv` | trailing 条件 + 1R 未到達 → 抑制 |
| 1R 未到達 Short | `donchian_1r_not_reached_short.csv` | 同 Short |
| ATR ゼロ | `donchian_atr_zero.csv` | 非発火 |

### Donchian Trend Evolve

| テスト | フィクスチャ | 検証内容 |
|--------|-------------|---------|
| カスタムパラメータ | `donchian_evolve_custom_params.csv` | DB パラメータ (entry=15, exit=8) |
| デフォルトフォールバック | `donchian_evolve_default_fallback.csv` | DB パラメータなし → デフォルト |
| 不正パラメータ | `donchian_evolve_invalid_params.csv` | 範囲外値 → クランプ |

### Squeeze Momentum

| テスト | フィクスチャ | 検証内容 |
|--------|-------------|---------|
| Long エントリー | `squeeze_long_entry.csv` | TTM Squeeze 解除 + momentum 上昇 |
| Short エントリー | `squeeze_short_entry.csv` | TTM Squeeze 解除 + momentum 下降 |
| Long Chandelier エグジット | `squeeze_long_chandelier_exit.csv` | HH - ATR×3 ブレイク |
| Short Chandelier エグジット | `squeeze_short_chandelier_exit.csv` | LL + ATR×3 |
| delay phase 抑制 | `squeeze_delay_phase_suppressed.csv` | 3bar 以内 → Chandelier 未評価 |
| 非発火 | `squeeze_no_signal.csv` | BB が KC 内 |
| 1R 未到達 Long | `squeeze_1r_not_reached_long.csv` | Chandelier 条件 + 1R 未到達 → 抑制 |
| 1R 未到達 Short | `squeeze_1r_not_reached_short.csv` | 同 Short |
| ATR ゼロ | `squeeze_atr_zero.csv` | 非発火 |
| 履歴不足 | `squeeze_history_insufficient.csv` | 非発火 |

### SwingLLM

| テスト | フィクスチャ | 検証内容 |
|--------|-------------|---------|
| Long エントリー | `swing_llm_entry_long.csv` | MockGemini Long 提案 → 発火 |
| Short エントリー | `swing_llm_entry_short.csv` | Short 提案 → 発火 |
| no_trade | `swing_llm_no_trade.csv` | Gemini "no_trade" → 非発火 |
| 不正レスポンス | `swing_llm_invalid_response.csv` | 不正 JSON → バックオフ + 非発火 |

### 共通シナリオ

| テスト | フィクスチャ | 検証内容 |
|--------|-------------|---------|
| SL ヒット Long | `sl_hit_long.csv` | SL まで下落 |
| SL ヒット Short | `sl_hit_short.csv` | SL まで上昇 |
| TP ヒット Long | `tp_hit_long.csv` | TP 到達 |
| TP ヒット Short | `tp_hit_short.csv` | TP 到達 |
| タイムリミット | `time_limit_expire.csv` | max_hold_until 超過 → 強制クローズ |
| 同時クローズ競合 | `concurrent_close.csv` | SL + 戦略エグジット同時 → CAS で片方のみ成功 |
| 鮮度ゲート拒否 | `stale_price.csv` | 古いタイムスタンプ → 発注拒否 |
| オーバーナイト手数料 | `overnight_fee.csv` | 日跨ぎ → 手数料適用 |
| オーバーナイト手数料ゼロ | `overnight_fee_zero.csv` | 手数料 0 |
| サイジング正常 | `position_size_normal.csv` | 通常 |
| 残高不足 | `position_size_insufficient.csv` | エントリー拒否 |
| min_lot 未満 | `position_size_below_min_lot.csv` | エントリー拒否 |
| warmup M5 | `warmup_m5.csv` | 200 本 M5 |
| warmup H1 | `warmup_h1.csv` | 200 本 H1 |
| warmup ゼロ | `warmup_zero.csv` | 空 → 待機 |
| キャンドル境界 | `candle_boundary.csv` | M5/H1 境界またぎ + bid/ask スタンプ |

### fill パス検証

| テスト | フィクスチャ | 検証内容 |
|--------|-------------|---------|
| fill_open Long | `fill_open_long.csv` | ask で約定 |
| fill_open Short | `fill_open_short.csv` | bid で約定 |
| fill_close Long | `fill_close_long.csv` | bid で約定 |
| fill_close Short | `fill_close_short.csv` | ask で約定 |
| fill_open Live | MockExchangeApi | API → poll_executions → 約定確認 |
| fill_close Live | MockExchangeApi | API → 反対売買 → 約定確認 |

### 実行系ガード・分岐

| テスト | 検証内容 |
|--------|---------|
| exchange-pair ガード | BitflyerCfd シグナル → GmoFx 口座にディスパッチされない |
| live gate | live.enabled=false → live 口座の発注拒否 |
| ポジション重複拒否 | 同一戦略+ペアで既にポジション open → スキップ |
| マッチなし | シグナルの戦略名に対応する口座なし → warning |
| マルチアカウント | 同一戦略の BitflyerCfd + GmoFx 口座に正しくディスパッチ |
| Live/Paper 分岐 | dry_run=true → PriceStore fill / dry_run=false → API fill |
| NullExchangeApi | dry_run 口座で API 未登録 → NullExchangeApi fallback |

### クローズフロー

| テスト | 検証内容 |
|--------|---------|
| CAS ロック取得 | closing 状態の trade に 2 番目の close → 失敗 |
| Phase 2 失敗 → ロック解除 | fill_close エラー → status を open にロールバック |
| Phase 3 失敗 → 通知 | DB 更新失敗 → Slack アラート |
| Trade status 遷移 | open→closing→closed のみ。closed→open は不可 |

### startup reconcile

| テスト | DB 状態 | 検証内容 |
|--------|---------|---------|
| noop | open + exchange ポジションあり | 何もしない |
| orphan | open + exchange ポジション空 | 強制クローズ + daily_summary 更新 |
| stale closing | closing + exchange ポジションあり | open に戻す |
| phase3 incomplete | closing + exchange ポジション空 | closed に完了 |
| API リトライ exhaustion | get_positions 3 回失敗 | bail |
| API エラー | get_positions 即エラー | bail |

### データ整合性

| テスト | 検証内容 |
|--------|---------|
| 残高整合性 | initial_balance + PnL - fees = current_balance |
| 日次集計整合性 | rebuild_daily_summary と incremental upsert の一致 |
| daily batch backfill | 起動時の過去 7 日分再計算 |
| account_events 記録 | margin_lock / margin_release / overnight_fee イベント |
| entry_indicators / regime | トレード open 時の indicators + regime 分類が DB に保存 |
| candle upsert 重複排除 | 同一タイムスタンプの candle → ON CONFLICT 上書き |
| JPY 小数点切り捨て | PnL の TRUNC(0) 処理 |
| Dashboard total count | get_trades の total が実レコード数と一致 |

### Price event ルーティング

| テスト | 検証内容 |
|--------|---------|
| Oanda → price_monitor_tx | Oanda PriceEvent が正しいチャネルに流れる |
| BitflyerCfd → crypto_price_tx | BitflyerCfd PriceEvent が正しいチャネルに流れる |
| GmoFx → crypto_price_tx | GmoFx PriceEvent が正しいチャネルに流れる |
| channel closed | receiver drop 時に sender が graceful stop |

### PriceStore

| テスト | 検証内容 |
|--------|---------|
| update 新しい tick | 正常更新 |
| update 古い tick | 無視される |
| last_tick_age | pair-only の全 exchange 横断 |
| last_tick_age_for | exchange-aware |
| latest_bid_ask | bid/ask 両方あり / 片方なし |
| health_at | Healthy / Stale / Missing |
| mid | 最新 tick の mid 価格 |

### 通知フォーマット

| テスト | 検証内容 |
|--------|---------|
| OrderFilled | exchange, pair, direction, price, quantity のフォーマット |
| OrderFailed | exchange, pair, error message |
| PositionClosed | exchange, pair, direction, entry_price, exit_price, pnl, exit_reason |
| Slack 送信エラー | webhook 失敗時にパニックしない |

### 周辺ジョブ

| テスト | 検証内容 |
|--------|---------|
| 週次バッチ | MockGemini 提案 → バリデーション（範囲クランプ） → DB 保存 → 通知 insert → MockVegapunk merge |
| 日次バッチ backfill | 起動時の過去 7 日分 daily_summary 再計算 |
| オーバーナイト手数料 | open ポジションに手数料適用 → 残高反映 |
| マクロアナリスト | MockHTTP ニュース取得 → MockGemini 要約 → MockVegapunk ingest / DB 保存 |
| enriched_ingest フォーマット | Vegapunk 送信テキストの全フィールド検証 |
| macro broadcast Lagged | engine の macro_rx が Lagged → warning + 継続 |
| macro broadcast Closed | engine の macro_rx が Closed → info + 継続 |

### シャットダウン

| テスト | 検証内容 |
|--------|---------|
| 全タスク drain | price_tx drop → engine → monitor → executor → recorder 順に終了 |
| timeout 内完了 | 5 秒以内に全ハンドル join |
| open ポジション保持 | シャットダウンで勝手にクローズしない |

### DB 接続プール

| テスト | 検証内容 |
|--------|---------|
| プール枯渇 | 全接続使用中 → 新規クエリがタイムアウト or 待機 |

## Phase 4: 外部 API 検証（`--features external-api`）

| テスト | 検証内容 |
|--------|---------|
| GMO FX ticker 正常取得 | パース成功 + USD_JPY/EUR_USD のシンボル確認 |
| GMO FX メンテナンス応答 | status=5 → data 空 → スキップ |
| GMO FX market CLOSED | 週末等 → キャンドルフラッシュ |
| BitFlyer WS 接続 | 正常接続 + tick 受信 |
| BitFlyer WS 切断 + 再接続 | 切断後に自動復帰 |
| CandleBuilder 期間境界（実 tick） | M5/H1 確定 |
| OANDA REST polling | 正常取得 + パース |
| Vegapunk ingest/search/feedback | gRPC 接続 + 操作成功 |
| 週次バッチ（実 Gemini） | パラメータ提案の妥当性 |

## Mocks

| モック | 挙動パターン |
|--------|-------------|
| `MockExchangeApi` | 正常約定 / タイムアウト / 約定遅延 / エラー / ポジション返却(reconcile) / 空ポジション / リトライ exhaustion |
| `MockGmoFxServer` | 正常 ticker / メンテナンス(status=5) / 未知ステータス / market CLOSED / 不正 JSON / HTTP 4xx-5xx / 接続拒否 / レスポンス遅延 |
| `MockBitflyerWs` | 正常 tick / 切断 / 再接続 / heartbeat timeout / 不正メッセージ / 連続切断バックオフ |
| `MockOandaServer` | 正常 polling / パースエラー / タイムアウト / HTTP エラー |
| `MockSlackWebhook` | ボディキャプチャ（OrderFilled / OrderFailed / PositionClosed） / 送信エラー |
| `MockVegapunk` | ingest 成功 / search 返却 / feedback / merge / 接続エラー |
| `MockGemini` | パラメータ提案 JSON / SwingLLM シグナル提案 / 不正レスポンス / タイムアウト |

## テスト失敗時の出力フォーマット

自動修正パイプライン（将来の vibepod 連携）に十分な情報を提供するため、失敗時は以下を出力する:

```
[FAIL] phase3::trade_flow::bb_mean_revert_long_entry
  test: crates/integration-tests/src/phase3/bb_mean_revert.rs:42
  fixture: fixtures/bb_long_entry.csv
  expected: 1 open trade with direction=Long
  actual: 0 trades

  === application log ===
  INFO  strategy warmup: fed 200 gmo_fx M5 candles for USD_JPY
  DEBUG skipping signal: pair USD_JPY not available on exchange BitflyerCfd for account 安全
  WARN  freshness gate rejected signal for FX 通常: PriceTickStale { age_secs: 120 }

  === db state ===
  trades: []
  trading_accounts: [{id: ..., name: "FX 通常", exchange: "gmo_fx", ...}]

  === git diff (last 1 commit) ===
  diff --git a/crates/market/src/gmo_fx.rs ...
```

含まれる情報:
- テスト名 + ソースファイル:行番号
- 使用フィクスチャ
- 期待値 vs 実際値
- テスト中の tracing ログ全文（warn/error だけでなく debug 含む）
- 失敗時の DB スナップショット（関連テーブル）
- 直近の git diff
- スタックトレース（パニック時）

## Fixtures

### 価格データ CSV

全 CSV は `timestamp, open, high, low, close, volume, bid, ask` 形式。各テストは BitflyerCfd / GmoFx の両 exchange でパラメタライズ実行。

フィクスチャファイル一覧は Phase 3 の各テーブルの「フィクスチャ」列を参照。合計約 60 ファイル。

### DB 状態フィクスチャ

Rust の seed 関数で構築。テストごとにクリーンな DB にシードデータを投入:
- trading_accounts: BitflyerCfd × 4 + GmoFx × 3（実運用構成）
- strategies: 全 5 戦略のカタログ行
- strategy_params: donchian_evolve 用パラメータ
- 各テストシナリオ固有の trades / account_events

### 設定フィクスチャ

| ファイル | 内容 |
|---------|------|
| `config_valid.toml` | テスト用正常 config |
| `config_unknown_exchange.toml` | 不正 exchange 名 |
| `config_missing_pairs.toml` | pairs 空 |
| `config_invalid_strategy.toml` | 存在しない戦略名 |
| `config_disabled_strategy.toml` | enabled=false |
| `config_env_override.toml` | env 上書き検証 |

## Scope

このスペックは結合テスト基盤のみ。以下は別スペックで設計:
- エラー検知 → vibepod 自動修正パイプライン（AutoFixLayer + vibepod 連携）
- 自動マージ判定ロジック
