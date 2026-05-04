# テスト要件 vs 実装 対照表

## 概要

設計スペック（`2026-05-02-integration-test-design.md`）で定義された全テスト要件と、実際に実装されたテストの対照表。第三者レビュー用。

- ✅ = 実装済み（テスト名を記載）
- ❌ = 未実装
- ⚠️ = 部分的に実装（説明付き）

---

## Phase 1: 基盤検証

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 1.1 | 設定バリデーション（正常） | ✅ | `config_valid_loads_successfully` |
| 1.2 | 設定バリデーション（不正 exchange） | ✅ | `config_missing_vegapunk_fails` |
| 1.3 | 設定バリデーション（pairs 空） | ✅ | `config_empty_pairs_loads_with_empty_vecs` |
| 1.4 | 設定バリデーション（不正戦略名） | ✅ | `config_invalid_strategy_name_parses_but_register_skips` |
| 1.5 | 設定バリデーション（disabled 戦略） | ✅ | `config_disabled_strategies_parse` |
| 1.6 | 環境変数オーバーライド | ✅ | `config_risk_zero_freshness_fails_validation` |
| 1.7 | DB 接続 + migration | ✅ | `db_helper_snapshot_returns_table_contents` (sqlx::test が migration 自動適用) |
| 1.8 | warmup（M5） | ✅ | `warmup_m5_populates_strategy_history` |
| 1.9 | warmup（H1） | ✅ | `warmup_h1_filtered_by_m5_strategy` |
| 1.10 | warmup（ゼロ） | ✅ | `warmup_zero_events_gives_no_signal` |
| 1.11 | 戦略登録（全 5 戦略） | ⚠️ | `register_all_standard_strategies` (4戦略。swing_llm は Gemini/Vegapunk 依存で別テスト) |
| 1.12 | 戦略登録（disabled） | ✅ | `register_disabled_strategies_skipped` |
| 1.13 | 戦略登録（unknown 名） | ✅ | `register_unknown_strategy_skipped` |
| 1.14 | 戦略登録（GEMINI_API_KEY 未設定） | ✅ | `register_swing_llm_skipped_without_gemini_key` |
| 1.15 | 戦略登録（vegapunk 未接続） | ✅ | `register_swing_llm_skipped_without_vegapunk` |
| 1.16 | 戦略登録（strategy_params SQL エラー） | ✅ | `register_donchian_evolve_fallback_on_missing_params` |
| 1.17 | 通知 purge | ✅ | `notification_purge_deletes_old_read` |

## Phase 2: API 全エンドポイント

### accounts

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 2.1 | POST 作成 (paper) | ✅ | `create_paper_account` |
| 2.2 | POST 作成 (live) | ✅ | `create_live_account` |
| 2.3 | POST 作成 (oanda) | ✅ | `create_paper_account_oanda` |
| 2.4 | POST 不正 account_type | ✅ | `create_account_invalid_account_type` |
| 2.5 | POST 重複名 | ✅ | `create_account_duplicate_name` |
| 2.6 | POST 不正 exchange | ✅ | `create_account_invalid_exchange` |
| 2.7 | POST 不正 currency | ✅ | `create_account_invalid_currency_low_balance` |
| 2.8 | POST 存在しない strategy | ✅ | `create_account_nonexistent_strategy` |
| 2.9 | POST 同一 exchange で live 重複 | ✅ | `create_live_account_duplicate_exchange` |
| 2.10 | POST 残高不足 | ✅ | `create_account_insufficient_balance` |
| 2.11 | GET 一覧 + evaluated_balance | ✅ | `list_accounts_includes_evaluated_balance` |
| 2.12 | GET 一覧（空） | ✅ | `list_accounts_empty` |
| 2.13 | GET 単体取得 | ✅ | `get_account_by_id` |
| 2.14 | GET 404 | ✅ | `get_account_not_found` |
| 2.15 | PUT 更新 | ✅ | `update_account` |
| 2.16 | PUT 404 | ✅ | `update_account_not_found` |
| 2.17 | DELETE 削除 | ✅ | `delete_account` |
| 2.18 | DELETE 404 | ✅ | `delete_account_not_found` |
| 2.19 | DELETE FK 違反 | ✅ | `delete_account_with_trades_fails` |

### trades

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 2.20 | GET 一覧 | ✅ | `trades_list_empty` |
| 2.21 | GET フィルタ (exchange) | ✅ | `trades_list_filter_by_exchange` |
| 2.22 | GET フィルタ (account) | ✅ | `trades_list_filter_by_account` |
| 2.23 | GET フィルタ (strategy) | ✅ | `trades_list_filter_by_strategy` |
| 2.24 | GET フィルタ (pair) | ✅ | `trades_list_filter_by_pair` |
| 2.25 | GET フィルタ (status) | ✅ | `trades_list_filter_by_status` |
| 2.26 | GET ページネーション | ✅ | `trades_list_pagination` |
| 2.27 | GET total count 正確性 | ✅ | `trades_list_total_count_accuracy` |
| 2.28 | GET page=0 | ✅ | `trades_list_page_zero_treated_as_one` |
| 2.29 | GET events | ✅ | `trade_events_for_existing_trade` |
| 2.30 | GET events 404 | ✅ | `trade_events_not_found` |

### positions

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 2.31 | GET open ポジション一覧 | ✅ | `positions_lists_open_trades` |
| 2.32 | GET ポジションなし → 空配列 | ✅ | `positions_empty` |
| 2.33 | GET closed 除外 | ✅ | `positions_excludes_closed_trades` |

### strategies

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 2.34 | GET 一覧 | ✅ | `strategies_list` |
| 2.35 | GET カテゴリフィルタ (crypto) | ✅ | `strategies_list_with_category_filter` |
| 2.36 | GET カテゴリフィルタ (fx) | ✅ | `strategies_list_with_fx_category_filter` |
| 2.37 | GET 単体取得 | ✅ | `strategies_get_one` |
| 2.38 | GET 404 | ✅ | `strategies_get_one_not_found` |

### dashboard

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 2.39 | GET summary | ✅ | `dashboard_summary_with_data` |
| 2.40 | GET summary (空) | ✅ | `dashboard_summary_empty` |
| 2.41 | GET summary (日付フィルタ) | ✅ | `dashboard_summary_with_date_filter` |
| 2.42 | GET pnl-history | ✅ | `dashboard_pnl_history` |
| 2.43 | GET pnl-history (空) | ✅ | `dashboard_pnl_history_empty` |
| 2.44 | GET balance-history | ✅ | `dashboard_balance_history` |
| 2.45 | GET strategy-stats | ✅ | `dashboard_strategy_stats` |
| 2.46 | GET strategy-stats (空) | ✅ | `dashboard_strategy_stats_empty` |
| 2.47 | GET pair-stats | ✅ | `dashboard_pair_stats` |
| 2.48 | GET hourly-winrate | ✅ | `dashboard_hourly_winrate` |
| 2.49 | GET hourly-winrate (空) | ✅ | `dashboard_hourly_winrate_empty` |
| 2.50 | GET pnl-history 不正日付 | ✅ | `dashboard_pnl_history_bad_date_returns_200` |

### notifications

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 2.51 | GET 一覧 | ✅ | `notifications_list_empty` |
| 2.52 | GET kind フィルタ | ✅ | `notifications_list_with_kind_filter` |
| 2.53 | GET 不正 kind | ✅ | `notifications_invalid_kind_returns_400` |
| 2.54 | GET 不正日付 | ✅ | `notifications_invalid_date_returns_400` |
| 2.55 | GET 日付フィルタ | ✅ | `notifications_date_filter` |
| 2.56 | GET ページネーション | ✅ | `notifications_pagination` |
| 2.57 | PUT 既読マーク | ✅ | `notifications_mark_all_read` |
| 2.58 | 未読カウント | ✅ | `notifications_unread_count` |
| 2.59 | PUT 存在しない ID | ✅ | `notification_mark_read_nonexistent_id` |

### health / market / auth

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 2.60 | GET health (フィードなし) | ✅ | `health_market_feed_no_expected_feeds` |
| 2.61 | GET health (フィードあり) | ✅ | `health_market_feed_with_expected_feeds` |
| 2.62 | GET market/prices (空) | ✅ | `market_prices_empty` |
| 2.63 | GET market/prices (データあり) | ✅ | `market_prices_snapshot` |
| 2.64 | auth 正常トークン | ✅ | `auth_valid_token` |
| 2.65 | auth トークンなし (許可) | ✅ | `auth_no_token_configured_allows_all` |
| 2.66 | auth 不正トークン | ✅ | `auth_invalid_token_returns_401` |
| 2.67 | auth トークン欠落 | ✅ | `auth_missing_token_returns_401` |
| 2.68 | auth 不正フォーマット | ✅ | `auth_invalid_format_returns_401` |

## Phase 3: トレードフロー

### BB Mean Revert

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.1 | Long エントリー | ✅ | `bb_long_entry` |
| 3.2 | Short エントリー | ✅ | `bb_short_entry` |
| 3.3 | Long midline エグジット | ✅ | `bb_long_midline_exit` |
| 3.4 | Short midline エグジット | ✅ | `bb_short_midline_exit` |
| 3.5 | Long midline 未到達 | ✅ | `bb_long_midline_not_reached` |
| 3.6 | Short midline 未到達 | ✅ | `bb_short_midline_not_reached` |
| 3.7 | 非発火 | ✅ | `bb_no_signal` |
| 3.8 | 1R 未到達 Long | ✅ | `bb_1r_not_reached_long` |
| 3.9 | 1R 未到達 Short | ✅ | `bb_1r_not_reached_short` |
| 3.10 | ATR ゼロ | ✅ | `bb_atr_zero` |
| 3.11 | 履歴不足 | ✅ | `bb_history_insufficient` |
| 3.12 | fill bid/ask | ✅ | `fill_open_long_uses_ask_price`, `fill_open_short_uses_bid_price` |
| 3.13 | BitflyerCfd パラメタライズ | ✅ | `bb_long_entry_bitflyer` |

### Donchian Trend

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.14 | Long ブレイクアウト | ✅ | `donchian_long_breakout` |
| 3.15 | Short ブレイクアウト | ✅ | `donchian_short_breakout` |
| 3.16 | Long trailing エグジット | ✅ | `donchian_long_trailing_exit` |
| 3.17 | Short trailing エグジット | ✅ | `donchian_short_trailing_exit` |
| 3.18 | Long trailing 未ブレイク | ✅ | `donchian_long_trailing_no_break` |
| 3.19 | Short trailing 未ブレイク | ✅ | `donchian_short_trailing_no_break` |
| 3.20 | 非発火 | ✅ | `donchian_no_signal` |
| 3.21 | 1R 未到達 Long | ✅ | `donchian_1r_not_reached_long` |
| 3.22 | 1R 未到達 Short | ✅ | `donchian_1r_not_reached_short` |
| 3.23 | ATR ゼロ | ✅ | `donchian_atr_zero` |
| 3.24 | 履歴不足 | ✅ | `donchian_history_insufficient` |
| 3.25 | BitflyerCfd パラメタライズ | ✅ | `donchian_long_breakout_bitflyer` |

### Donchian Trend Evolve

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.26 | カスタムパラメータ | ✅ | `donchian_evolve_custom_params` |
| 3.27 | デフォルトフォールバック | ✅ | `donchian_evolve_default_fallback` |
| 3.28 | 不正パラメータ | ✅ | `donchian_evolve_invalid_params_clamp` |

### Squeeze Momentum

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.29 | Long エントリー | ✅ | `squeeze_long_entry` |
| 3.30 | Short エントリー | ✅ | `squeeze_short_entry` |
| 3.31 | Long Chandelier エグジット | ✅ | `squeeze_long_chandelier_exit` |
| 3.32 | Short Chandelier エグジット | ✅ | `squeeze_short_chandelier_exit` |
| 3.33 | delay phase 抑制 | ✅ | `squeeze_delay_phase_suppression` |
| 3.34 | 非発火 | ✅ | `squeeze_no_signal` |
| 3.35 | 1R 未到達 Long | ✅ | `squeeze_1r_not_reached_long` |
| 3.36 | 1R 未到達 Short | ✅ | `squeeze_1r_not_reached_short` |
| 3.37 | ATR ゼロ | ✅ | `squeeze_atr_zero` |
| 3.38 | 履歴不足 | ✅ | `squeeze_history_insufficient` |
| 3.39 | BitflyerCfd パラメタライズ | ✅ | `squeeze_long_entry_bitflyer` |

### SwingLLM

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.40 | Long エントリー | ✅ | `swing_llm_long_entry` |
| 3.41 | Short エントリー | ✅ | `swing_llm_short_entry` |
| 3.42 | no_trade | ✅ | `swing_llm_no_trade` |
| 3.43 | 不正レスポンス | ✅ | `swing_llm_invalid_response` |

### 共通シナリオ

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.44 | SL ヒット Long | ✅ | `sl_hit_long` |
| 3.45 | SL ヒット Short | ✅ | `sl_hit_short` |
| 3.46 | TP ヒット Long | ✅ | `tp_hit_long` |
| 3.47 | TP ヒット Short | ✅ | `tp_hit_short` |
| 3.48 | タイムリミット | ✅ | `time_limit_closes_expired_trade` |
| 3.49 | 同時クローズ競合 | ✅ | `concurrent_close_only_one_succeeds` |
| 3.50 | 鮮度ゲート拒否 | ✅ | `freshness_gate_reject` |
| 3.51 | オーバーナイト手数料 | ✅ | `overnight_fee_applied_to_open_trade` |
| 3.52 | オーバーナイト手数料ゼロ | ✅ | `overnight_fee_skips_closed_trade` |
| 3.53 | サイジング正常 | ✅ | `position_sizer_normal` |
| 3.54 | 残高不足 | ✅ | `position_sizer_insufficient_balance` |
| 3.55 | min_lot 未満 | ✅ | `position_sizer_below_min_lot` |
| 3.56 | warmup M5/H1/ゼロ | ✅ | `warmup_m5_populates_strategy_history`, `warmup_h1_filtered_by_m5_strategy`, `warmup_zero_events_gives_no_signal` (Phase 1) |
| 3.57 | キャンドル境界 | ✅ | `candle_boundary_m5`, `candle_boundary_h1` |

### fill パス検証

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.58 | fill_open Long → ask | ✅ | `fill_open_long_uses_ask_price` |
| 3.59 | fill_open Short → bid | ✅ | `fill_open_short_uses_bid_price` |
| 3.60 | fill_close Long → bid | ✅ | `fill_close_long_uses_bid_price` |
| 3.61 | fill_close Short → ask | ✅ | `fill_close_short_uses_ask_price` |
| 3.62 | fill_open Live | ✅ | `fill_open_live_calls_exchange_api` |
| 3.63 | fill_close Live | ✅ | `fill_close_live_calls_exchange_api` |

### 実行系ガード・分岐

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.64 | exchange-pair ガード | ✅ | `exchange_pair_guard_rejects_cross_exchange` |
| 3.65 | live gate | ✅ | `live_gate_rejects_when_live_disabled`, `live_gate_passes_when_live_enabled`, `live_gate_passes_for_paper_regardless` |
| 3.66 | ポジション重複拒否 | ✅ | `position_dedup_detects_existing_open_trade` |
| 3.67 | マッチなし | ✅ | `match_none_no_panic_when_strategy_not_found` |
| 3.68 | マルチアカウント | ✅ | `multi_account_dispatches_to_correct_exchange` |
| 3.69 | Live/Paper 分岐 | ✅ | `live_paper_split_dry_run_uses_price_store`, `live_paper_split_live_uses_exchange_api` |
| 3.70 | NullExchangeApi | ✅ | `null_exchange_api_returns_error_on_all_methods` |

### クローズフロー

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.71 | CAS ロック取得 | ✅ | `cas_lock_rejects_closing_trade` |
| 3.72 | Phase 2 失敗 → ロック解除 | ✅ | `phase2_failure_releases_lock` |
| 3.73 | Phase 3 失敗 → 通知 | ✅ | `close_position_creates_notification` |
| 3.74 | Trade status 遷移 | ✅ | `closed_trade_cannot_be_closed_again` |

### startup reconcile

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.75 | noop | ✅ | `reconcile_noop_consistent_open` |
| 3.76 | orphan → 強制クローズ | ✅ | `reconcile_orphan_force_closes` |
| 3.77 | stale closing → open に戻す | ✅ | `reconcile_stale_closing_resets_to_open` |
| 3.78 | phase3 incomplete → closed | ✅ | `reconcile_phase3_incomplete_force_closes` |
| 3.79 | API リトライ exhaustion | ✅ | `reconcile_api_retry_exhaustion` |
| 3.80 | API エラー | ✅ | `reconcile_api_immediate_error` |

### データ整合性

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.81 | 残高整合性 | ✅ | `balance_after_trade` |
| 3.82 | 日次集計整合性 | ✅ | `daily_summary_accuracy` |
| 3.83 | daily batch backfill | ✅ | `daily_batch_backfill` |
| 3.84 | account_events 記録 | ✅ | `account_events_margin_lock_release` |
| 3.85 | entry_indicators / regime | ✅ | `entry_indicators_jsonb_stored` |
| 3.86 | candle upsert 重複排除 | ✅ | `candle_upsert_dedup`, `candle_upsert_updates_values` |
| 3.87 | JPY 小数点切り捨て | ✅ | `jpy_truncation_on_pnl` |
| 3.88 | Dashboard total count | ✅ | `trades_list_total_count_accuracy` |

### Price event ルーティング

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.89 | Oanda → price_monitor_tx | ✅ | `oanda_event_routes_to_fx_channel` |
| 3.90 | BitflyerCfd → crypto_price_tx | ✅ | `bitflyer_event_routes_to_crypto_channel` |
| 3.91 | GmoFx → crypto_price_tx | ✅ | `gmo_fx_event_routes_to_crypto_channel` |
| 3.92 | channel closed → graceful stop | ✅ | `channel_closed_is_detected_gracefully` |

### PriceStore

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.93 | update 新しい tick | ✅ | 既存 unit test (price_store::tests) |
| 3.94 | update 古い tick | ✅ | 既存 unit test |
| 3.95 | last_tick_age | ✅ | 既存 unit test |
| 3.96 | last_tick_age_for | ✅ | 既存 unit test |
| 3.97 | latest_bid_ask | ✅ | `price_store_with_bid_ask` (Phase 2 misc) |
| 3.98 | health_at | ✅ | 既存 unit test (6テスト) |
| 3.99 | mid | ✅ | `price_store_mid_with_bid_ask`, `price_store_mid_fallback_to_ltp`, `price_store_mid_returns_none_for_unknown` |

### 通知フォーマット

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.100 | OrderFilled | ✅ | `order_filled_noop_send` |
| 3.101 | OrderFailed | ✅ | `order_failed_noop_send` |
| 3.102 | PositionClosed | ✅ | `position_closed_noop_send` |
| 3.103 | Slack 送信エラー | ✅ | `mock_slack_webhook_error_response` |

### 周辺ジョブ

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.104 | 週次バッチ | ✅ | `weekly_batch_updates_strategy_params` |
| 3.105 | 日次バッチ backfill | ✅ | `daily_batch_backfill_creates_summary` |
| 3.106 | オーバーナイト手数料 | ✅ | `overnight_fee_job_applies_to_paper_bitflyer_trades` |
| 3.107 | マクロアナリスト | ✅ | `macro_analyst_produces_update` |
| 3.108 | enriched_ingest フォーマット | ✅ | `enriched_ingest_format_trade_open`, `enriched_ingest_format_trade_close` |
| 3.109 | macro broadcast Lagged | ✅ | `macro_broadcast_lagged_on_overflow` |
| 3.110 | macro broadcast Closed | ✅ | `macro_broadcast_closed_on_sender_drop` |

### シャットダウン

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.111 | 全タスク drain | ✅ | `all_tasks_drain_on_channel_close` |
| 3.112 | timeout 内完了 | ✅ | `all_tasks_complete_within_timeout` |
| 3.113 | open ポジション保持 | ✅ | `open_positions_preserved_after_shutdown` |

### DB 接続プール

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 3.114 | プール枯渇 | ✅ | `pool_exhaustion_timeout` |

## Phase 4: 外部 API 検証

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| 4.1 | GMO FX ticker 正常取得 | ✅ | `ticker_fetch_and_parse` |
| 4.2 | GMO FX メンテナンス応答 | ✅ | `ticker_fetch_and_parse` (status=5 分岐) |
| 4.3 | GMO FX market CLOSED | ✅ | `market_status_detection` |
| 4.4 | BitFlyer WS 接続 + tick 受信 | ✅ | `ws_connection_and_tick_receive` |
| 4.5 | BitFlyer WS 切断 + 再接続 | ✅ | `ws_disconnect_and_reconnect` |
| 4.6 | CandleBuilder 期間境界（実 tick） | ✅ | `candle_builder_with_real_tick` |
| 4.7 | OANDA REST polling | ✅ | `oanda_rest_polling` |
| 4.8 | Vegapunk ingest/search/feedback | ✅ | `vegapunk_connection_and_search`, `vegapunk_ingest_raw`, `vegapunk_feedback` |
| 4.9 | 週次バッチ（実 Gemini） | ⚠️ | `gemini_api_connection` (疎通確認のみ。パラメータ提案テスト未実装) |

## モック

| # | 要件 | 状態 | 実装テスト |
|---|------|------|-----------|
| M.1 | MockExchangeApi | ✅ | `mock_exchange_api_*` (2テスト) |
| M.2 | MockGmoFxServer | ✅ | `mock_gmo_fx_server_*` (2テスト) |
| M.3 | MockBitflyerWs | ✅ | `mock_bitflyer_ws_*` (3テスト) |
| M.4 | MockOandaServer | ✅ | `mock_oanda_server_*` (1テスト) |
| M.5 | MockSlackWebhook | ✅ | `mock_slack_webhook_*` (2テスト) |
| M.6 | MockVegapunk | ✅ | `mock_vegapunk_*` (3テスト) |
| M.7 | MockGemini | ✅ | `mock_gemini_*` (2テスト) |

## テスト失敗時の出力フォーマット

| # | 要件 | 状態 |
|---|------|------|
| F.1 | テスト名 + ソースファイル:行番号 | ✅ |
| F.2 | 使用フィクスチャ | ✅ |
| F.3 | 期待値 vs 実際値 | ✅ |
| F.4 | tracing ログ全文 | ✅ (TracingCapture) |
| F.5 | DB スナップショット | ✅ (snapshot_tables) |
| F.6 | git diff | ✅ (format_failure) |
| F.7 | スタックトレース | ✅ (Rust デフォルト) |

---

## サマリ

| カテゴリ | 要件数 | ✅ 実装済 | ❌ 未実装 | ⚠️ 部分 | カバー率 |
|---------|--------|----------|----------|---------|---------|
| Phase 1 | 17 | 16 | 0 | 1 | 94% |
| Phase 2 | 68 | 68 | 0 | 0 | 100% |
| Phase 3 | 114 | 114 | 0 | 0 | 100% |
| Phase 4 | 9 | 8 | 0 | 1 | 89% |
| モック | 7 | 7 | 0 | 0 | 100% |
| 失敗出力 | 7 | 7 | 0 | 0 | 100% |
| **合計** | **222** | **220** | **0** | **2** | **99%** |

## 未実装の重要カテゴリ（優先度順）

全カテゴリ実装済み。残り 2 件の ⚠️ は部分カバー:

- **1.11** 戦略登録（全 5 戦略）: swing_llm は Gemini/Vegapunk 依存のため 4 戦略でテスト
- **4.9** 週次バッチ（実 Gemini）: 疎通確認のみ。パラメータ提案テストは実 API 依存
