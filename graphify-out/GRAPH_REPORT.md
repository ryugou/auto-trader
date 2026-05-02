# Graph Report - /Users/ryugo/Developer/src/personal/auto-trader  (2026-04-30)

## Corpus Check
- 134 files · ~146,225 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 1183 nodes · 2133 edges · 51 communities detected
- Extraction: 72% EXTRACTED · 14% INFERRED · 0% AMBIGUOUS · INFERRED: 291 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- [[_COMMUNITY_Exchange APIs & Config|Exchange APIs & Config]]
- [[_COMMUNITY_Test Suite|Test Suite]]
- [[_COMMUNITY_Indicators & Backtest|Indicators & Backtest]]
- [[_COMMUNITY_Account Management API|Account Management API]]
- [[_COMMUNITY_Trade Execution & Errors|Trade Execution & Errors]]
- [[_COMMUNITY_Market Feeds & Strategies|Market Feeds & Strategies]]
- [[_COMMUNITY_API Layer & State|API Layer & State]]
- [[_COMMUNITY_App Core & Startup|App Core & Startup]]
- [[_COMMUNITY_Trade Recording & Notifications|Trade Recording & Notifications]]
- [[_COMMUNITY_Dashboard UI Components|Dashboard UI Components]]
- [[_COMMUNITY_Startup Reconciliation|Startup Reconciliation]]
- [[_COMMUNITY_Core Type System|Core Type System]]
- [[_COMMUNITY_Dashboard Data Queries|Dashboard Data Queries]]
- [[_COMMUNITY_Price Store & Health|Price Store & Health]]
- [[_COMMUNITY_BB Mean Revert Strategy|BB Mean Revert Strategy]]
- [[_COMMUNITY_Weekly Batch & Evolution|Weekly Batch & Evolution]]
- [[_COMMUNITY_Donchian Trend Evolve|Donchian Trend Evolve]]
- [[_COMMUNITY_Donchian Trend Base|Donchian Trend Base]]
- [[_COMMUNITY_Strategy Engine Dispatch|Strategy Engine Dispatch]]
- [[_COMMUNITY_OANDA Private API|OANDA Private API]]
- [[_COMMUNITY_Squeeze Momentum Strategy|Squeeze Momentum Strategy]]
- [[_COMMUNITY_GMO FX Feed|GMO FX Feed]]
- [[_COMMUNITY_Candle Builder|Candle Builder]]
- [[_COMMUNITY_BitFlyer WebSocket Monitor|BitFlyer WebSocket Monitor]]
- [[_COMMUNITY_Position Sizing & Risk|Position Sizing & Risk]]
- [[_COMMUNITY_Design Specs & Plans|Design Specs & Plans]]
- [[_COMMUNITY_Daily Summary Cache|Daily Summary Cache]]
- [[_COMMUNITY_Vegapunk Client|Vegapunk Client]]
- [[_COMMUNITY_Misc 29|Misc 29]]
- [[_COMMUNITY_Misc 30|Misc 30]]
- [[_COMMUNITY_Misc 32|Misc 32]]
- [[_COMMUNITY_Misc 33|Misc 33]]
- [[_COMMUNITY_Misc 35|Misc 35]]
- [[_COMMUNITY_Misc 37|Misc 37]]
- [[_COMMUNITY_Misc 38|Misc 38]]
- [[_COMMUNITY_Misc 40|Misc 40]]
- [[_COMMUNITY_Misc 41|Misc 41]]
- [[_COMMUNITY_Misc 43|Misc 43]]
- [[_COMMUNITY_Misc 44|Misc 44]]
- [[_COMMUNITY_Misc 45|Misc 45]]
- [[_COMMUNITY_Misc 47|Misc 47]]
- [[_COMMUNITY_Misc 48|Misc 48]]
- [[_COMMUNITY_Misc 50|Misc 50]]
- [[_COMMUNITY_Misc 51|Misc 51]]
- [[_COMMUNITY_Misc 52|Misc 52]]
- [[_COMMUNITY_Misc 75|Misc 75]]
- [[_COMMUNITY_Misc 76|Misc 76]]
- [[_COMMUNITY_Misc 77|Misc 77]]
- [[_COMMUNITY_Misc 78|Misc 78]]
- [[_COMMUNITY_Misc 79|Misc 79]]
- [[_COMMUNITY_Misc 80|Misc 80]]

## God Nodes (most connected - your core abstractions)
1. `main()` - 27 edges
2. `api()` - 17 edges
3. `Strategy trait (on_price, on_open_positions, warmup)` - 16 edges
4. `seed_live_account()` - 15 edges
5. `Trader` - 15 edges
6. `client_for()` - 15 edges
7. `BitflyerPrivateApi` - 15 edges
8. `atr()` - 14 edges
9. `key()` - 14 edges
10. `Trade struct` - 13 edges

## Surprising Connections (you probably didn't know these)
- `reconcile_one_account()` --calls--> `list_open_or_closing_by_account()`  [INFERRED]
  crates/app/src/startup_reconcile.rs → crates/db/src/trades.rs
- `main()` --calls--> `list_strategy_names()`  [INFERRED]
  crates/app/src/main.rs → crates/db/src/strategies.rs
- `main()` --calls--> `purge_old_read()`  [INFERRED]
  crates/app/src/main.rs → crates/db/src/notifications.rs
- `router()` --calls--> `get()`  [INFERRED]
  crates/app/src/api/mod.rs → dashboard-ui/src/api/client.ts
- `router()` --calls--> `post()`  [INFERRED]
  crates/app/src/api/mod.rs → dashboard-ui/src/api/client.ts

## Communities

### Community 0 - "Exchange APIs & Config"
Cohesion: 0.03
Nodes (56): auth_headers_contain_key_timestamp_and_sign(), BitflyerApiError, BitflyerErrorBody, BitflyerPrivateApi, ChildOrder, ChildOrderState, ChildOrderType, Collateral (+48 more)

### Community 1 - "Test Suite"
Cohesion: 0.07
Nodes (71): Candle, indicators(), get_open_trades_by_account(), Trade, cancel_child_order_returns_unit_on_200(), cancel_child_order_unknown_id_maps_to_order_not_found(), client_for(), get_child_orders_empty_list_is_ok() (+63 more)

### Community 2 - "Indicators & Backtest"
Cohesion: 0.04
Nodes (77): adx, atr, backtest, BacktestReport, BacktestRunner, BbMeanRevertV1 strategy, BbMeanRevertV1, BitflyerMonitor (+69 more)

### Community 3 - "Account Management API"
Cohesion: 0.04
Nodes (59): AccountWithBalance, create(), get_one(), list(), remove(), update(), authHeaders(), del() (+51 more)

### Community 4 - "Trade Execution & Errors"
Cohesion: 0.03
Nodes (74): account_events, acquire_close_lock, aggregate_executions, ApiError, api::router, apply_overnight_fee, auth_middleware, BalanceHistoryAccount (+66 more)

### Community 5 - "Market Feeds & Strategies"
Cohesion: 0.07
Nodes (39): BitflyerMonitor, connect_and_stream(), emit_candle_event(), emit_candle_event_populates_indicators_for_primary_timeframe(), h1_builder_does_not_emit_mid_period(), h1_builder_emits_on_period_boundary(), JsonRpcMessage, TickerMessage (+31 more)

### Community 6 - "API Layer & State"
Cohesion: 0.05
Nodes (57): API: accounts CRUD endpoints, API: GET /api/health/market-feed, API: GET /api/market/prices, API: GET /api/positions, AppConfig (TOML configuration root), BitflyerConfig (with secret redaction), BitflyerPrivateApi, BitflyerPrivateApi (+49 more)

### Community 7 - "App Core & Startup"
Cohesion: 0.05
Nodes (22): risk_gate (eval_price_freshness), AuthInterceptor, VegapunkClient, compute_feedback_rating(), format_trade_close(), format_trade_open(), exchange_from_str(), main() (+14 more)

### Community 8 - "Trade Recording & Notifications"
Cohesion: 0.08
Nodes (24): events(), insert_trade_opened(), aggregate_executions(), exchange_side_to_direction(), Trader, truncate_yen(), acquire_close_lock(), AcquireLockRow (+16 more)

### Community 9 - "Dashboard UI Components"
Cohesion: 0.11
Nodes (36): AccountForm, Accounts Page, Analysis Page, API Client, API Types (TypeScript), BalanceChart, DashboardFilter, Dashboard UI (+28 more)

### Community 10 - "Startup Reconciliation"
Cohesion: 0.15
Nodes (19): Direction enum (Long/Short), has_reached_one_r() risk check, build_apis(), db_closing_exchange_empty_completes_phase3(), db_closing_exchange_has_position_resets_to_open(), db_open_exchange_empty_force_closes_db(), db_open_exchange_has_position_is_noop(), empty_price_store() (+11 more)

### Community 11 - "Core Type System"
Cohesion: 0.08
Nodes (15): Candle, default_allocation_pct(), Direction, Exchange, ExitReason, Pair, pair_display_format(), Position (+7 more)

### Community 12 - "Dashboard Data Queries"
Cohesion: 0.08
Nodes (27): balance_history(), BalanceHistoryResponse, hourly_winrate(), pairs(), pnl_history(), strategies(), summary(), SummaryResponse (+19 more)

### Community 13 - "Price Store & Health"
Cohesion: 0.21
Nodes (20): FeedHealth, FeedKey, FeedStatus, health_clamps_future_timestamp_to_zero(), health_healthy_at_exactly_60s(), health_healthy_when_tick_within_60s(), health_missing_when_no_tick(), health_only_reports_expected_feeds() (+12 more)

### Community 14 - "BB Mean Revert Strategy"
Cohesion: 0.16
Nodes (16): BbMeanRevertV1, ignores_non_m5_timeframe(), long_signal_at_oversold_extreme_with_capitulation(), make_event(), make_position_with_sl(), no_signal_until_history_warmed(), open_positions_close_at_mean(), open_positions_ignore_other_strategies() (+8 more)

### Community 15 - "Weekly Batch & Evolution"
Cohesion: 0.12
Nodes (24): build_gemini_prompt(), build_gemini_prompt_contains_key_sections(), build_gemini_prompt_no_vegapunk_context(), call_gemini(), compute_regime_wilson(), extract_json(), extract_json_strips_code_fence(), extract_json_strips_plain_fence() (+16 more)

### Community 16 - "Donchian Trend Evolve"
Cohesion: 0.21
Nodes (13): constructor_ignores_legacy_sl_allocation_params(), constructor_parses_json_params(), constructor_uses_defaults_for_missing_keys(), default_params(), DonchianTrendEvolveV1, long_breakout_with_atr_based_sl(), make_event(), no_signal_with_insufficient_history() (+5 more)

### Community 17 - "Donchian Trend Base"
Cohesion: 0.26
Nodes (10): DonchianTrendV1, ignores_non_h1_timeframe(), long_breakout_above_channel_with_volatility_expansion(), make_event(), make_position_with_sl(), no_signal_with_insufficient_history(), open_positions_close_on_trailing_channel_break(), open_positions_no_exit_when_1r_not_reached() (+2 more)

### Community 18 - "Strategy Engine Dispatch"
Cohesion: 0.14
Nodes (8): Position struct (wraps Trade), MacroRecorder, on_macro_update_broadcasts_to_all_strategies(), on_macro_update_skips_nothing_even_if_disabled(), StrategyEngine, StrategySlot, warmup_dispatches_to_disabled_strategies_too(), WarmupRecorder

### Community 19 - "OANDA Private API"
Cohesion: 0.25
Nodes (7): OandaApiError, OandaPrivateApi, order_json_to_child(), parse_error_redacts_account_id_in_body(), positions_path_escapes_special_chars(), send_json_redacts_account_id_even_past_truncation_boundary(), send_json_redacts_account_id_in_error_body()

### Community 20 - "Squeeze Momentum Strategy"
Cohesion: 0.2
Nodes (16): bb_mean_revert_v1, bitFlyer CFD, Crypto Paper Trading Spec, System Design Spec, donchian_trend_evolve_v1, donchian_trend_v1, fx-trading (Vegapunk Schema), OANDA (+8 more)

### Community 21 - "GMO FX Feed"
Cohesion: 0.2
Nodes (9): format_for_slack(), format_order_filled(), Notifier, NotifyError, NotifyEvent, OrderFailedEvent, OrderFilledEvent, PositionClosedEvent (+1 more)

### Community 22 - "Candle Builder"
Cohesion: 0.15
Nodes (9): AppState, CandleBuilder, Crypto Paper Trading Plan, PriceStore, Market Feed Health & Positions Design, gmo_fx_feed_new_has_no_pool(), GmoFxFeed, TickerData (+1 more)

### Community 23 - "BitFlyer WebSocket Monitor"
Cohesion: 0.21
Nodes (3): BacktestReport, BacktestRunner, SimTrader

### Community 24 - "Position Sizing & Risk"
Cohesion: 0.19
Nodes (14): App, dashboard-ui, dashboard-ui/src/assets/, dashboard-ui/public/, favicon.svg (Vite lightning bolt), hero.png (3D isometric app icon template), icons.svg (SVG sprite sheet), MarketFeedHealthBanner (+6 more)

### Community 25 - "Design Specs & Plans"
Cohesion: 0.5
Nodes (9): btc_sizer(), full_allocation_with_risk_limiting(), half_allocation_with_moderate_leverage(), PositionSizer, rejects_when_account_too_small_for_one_min_lot(), rejects_zero_or_negative_inputs(), risk_adjustment_caps_high_leverage(), the_30k_donchian_case_with_proper_risk_limiting() (+1 more)

### Community 26 - "Daily Summary Cache"
Cohesion: 0.18
Nodes (8): GeminiCandidate, GeminiContent, GeminiContentResponse, GeminiPart, GeminiPartResponse, GeminiRequest, GeminiResponse, GeminiSummarizer

### Community 27 - "Vegapunk Client"
Cohesion: 0.38
Nodes (4): builds_candle_from_ticks(), CandleBuilder, empty_period_returns_none(), period_boundary_completes_previous_candle()

### Community 29 - "Misc 29"
Cohesion: 0.29
Nodes (2): MacroAnalyst, insert_macro_event()

### Community 30 - "Misc 30"
Cohesion: 0.25
Nodes (1): NullExchangeApi

### Community 32 - "Misc 32"
Cohesion: 0.47
Nodes (2): NewsFetcher, NewsItem

### Community 33 - "Misc 33"
Cohesion: 0.47
Nodes (2): EconomicCalendar, EconomicEvent

### Community 35 - "Misc 35"
Cohesion: 0.4
Nodes (4): PriceEvent, SignalEvent, TradeAction, TradeEvent

### Community 37 - "Misc 37"
Cohesion: 0.6
Nodes (4): buildGroups(), exchangeGroup(), jstDateString(), periodToRange()

### Community 38 - "Misc 38"
Cohesion: 0.5
Nodes (2): MarketPrice, MarketPricesResponse

### Community 40 - "Misc 40"
Cohesion: 0.67
Nodes (2): jstDateString(), periodToRange()

### Community 41 - "Misc 41"
Cohesion: 0.67
Nodes (2): describe(), formatAgeMinutes()

### Community 43 - "Misc 43"
Cohesion: 0.67
Nodes (2): useStrategyCatalogQuery(), useStrategyRiskLookup()

### Community 44 - "Misc 44"
Cohesion: 0.67
Nodes (1): MarketFeedHealthResponse

### Community 45 - "Misc 45"
Cohesion: 0.67
Nodes (2): DashboardFilter, TradeFilter

### Community 47 - "Misc 47"
Cohesion: 1.0
Nodes (3): ATR-based Dynamic Stop Loss, Risk-linked Position Sizing, Strategy Tuning Design

### Community 48 - "Misc 48"
Cohesion: 1.0
Nodes (1): OrderExecutor

### Community 50 - "Misc 50"
Cohesion: 1.0
Nodes (1): MarketDataProvider

### Community 51 - "Misc 51"
Cohesion: 1.0
Nodes (1): MarketFeed

### Community 52 - "Misc 52"
Cohesion: 1.0
Nodes (1): ExchangeApi

### Community 75 - "Misc 75"
Cohesion: 1.0
Nodes (1): EconomicEvent

### Community 76 - "Misc 76"
Cohesion: 1.0
Nodes (1): ChildOrderState

### Community 77 - "Misc 77"
Cohesion: 1.0
Nodes (1): ChildOrderType

### Community 78 - "Misc 78"
Cohesion: 1.0
Nodes (1): Side

### Community 79 - "Misc 79"
Cohesion: 1.0
Nodes (1): OandaApiError

### Community 80 - "Misc 80"
Cohesion: 1.0
Nodes (1): TradeRow

## Knowledge Gaps
- **118 isolated node(s):** `OrderFilledEvent`, `OrderFailedEvent`, `PositionClosedEvent`, `MacroUpdate`, `ExitSignal` (+113 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **Thin community `Misc 29`** (8 nodes): `macro_events.rs`, `analyst.rs`, `MacroAnalyst`, `.new()`, `.run()`, `.with_db()`, `.with_vegapunk()`, `insert_macro_event()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 30`** (8 nodes): `null_exchange_api.rs`, `NullExchangeApi`, `.cancel_child_order()`, `.get_child_orders()`, `.get_collateral()`, `.get_executions()`, `.get_positions()`, `.send_child_order()`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 32`** (6 nodes): `news.rs`, `NewsFetcher`, `.fetch_feed()`, `.fetch_latest()`, `.new()`, `NewsItem`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 33`** (6 nodes): `calendar.rs`, `EconomicCalendar`, `.default()`, `.fetch_upcoming()`, `.new()`, `EconomicEvent`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 38`** (4 nodes): `MarketPrice`, `MarketPricesResponse`, `prices()`, `market.rs`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 40`** (4 nodes): `jstDateString()`, `PageFilters()`, `periodToRange()`, `PageFilters.tsx`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 41`** (4 nodes): `describe()`, `formatAgeMinutes()`, `MarketFeedHealthBanner()`, `MarketFeedHealthBanner.tsx`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 43`** (4 nodes): `RiskBadge()`, `useStrategyCatalogQuery()`, `useStrategyRiskLookup()`, `RiskBadge.tsx`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 44`** (3 nodes): `market_feed()`, `MarketFeedHealthResponse`, `health.rs`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 45`** (3 nodes): `DashboardFilter`, `TradeFilter`, `filters.rs`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 48`** (2 nodes): `executor.rs`, `OrderExecutor`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 50`** (2 nodes): `provider.rs`, `MarketDataProvider`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 51`** (2 nodes): `market_feed.rs`, `MarketFeed`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 52`** (2 nodes): `exchange_api.rs`, `ExchangeApi`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 75`** (1 nodes): `EconomicEvent`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 76`** (1 nodes): `ChildOrderState`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 77`** (1 nodes): `ChildOrderType`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 78`** (1 nodes): `Side`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 79`** (1 nodes): `OandaApiError`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.
- **Thin community `Misc 80`** (1 nodes): `TradeRow`
  Too small to be a meaningful cluster - may be noise or needs more connections extracted.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `main()` connect `App Core & Startup` to `Exchange APIs & Config`, `Test Suite`, `Account Management API`, `Market Feeds & Strategies`, `Startup Reconciliation`?**
  _High betweenness centrality (0.161) - this node is a cross-community bridge._
- **Why does `UnifiedTrader (executor)` connect `API Layer & State` to `Candle Builder`?**
  _High betweenness centrality (0.095) - this node is a cross-community bridge._
- **Why does `Strategy trait (on_price, on_open_positions, warmup)` connect `Indicators & Backtest` to `Strategy Engine Dispatch`, `API Layer & State`?**
  _High betweenness centrality (0.060) - this node is a cross-community bridge._
- **Are the 25 inferred relationships involving `main()` (e.g. with `.fmt()` and `.new()`) actually correct?**
  _`main()` has 25 INFERRED edges - model-reasoned connections that need verification._
- **Are the 5 inferred relationships involving `Strategy trait (on_price, on_open_positions, warmup)` (e.g. with `SwingLLMv1 strategy` and `BbMeanRevertV1 strategy`) actually correct?**
  _`Strategy trait (on_price, on_open_positions, warmup)` has 5 INFERRED edges - model-reasoned connections that need verification._
- **What connects `OrderFilledEvent`, `OrderFailedEvent`, `PositionClosedEvent` to the rest of the system?**
  _118 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Exchange APIs & Config` be split into smaller, more focused modules?**
  _Cohesion score 0.03 - nodes in this community are weakly interconnected._