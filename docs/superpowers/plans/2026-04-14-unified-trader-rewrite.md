# Unified Trader Rewrite — 3 PR 計画

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 現状の「ペーパー約定 = signal 価格で理想 fill」を廃止し、paper/live を **同一 Trader 実装** で統一する。両者の唯一の違いは `dry_run: bool` フラグ (paper 口座=true, live 口座=false)。dry_run=true は WebSocket で観測した bid/ask で模擬約定、dry_run=false は bitFlyer API で実発注 → 実約定価格を採用。

**Architecture principle:** 戦略は「いつ、どの方向に、どのくらいの比率で」エントリーするかだけ決める。**約定価格の決定は Trader の責務**。dry_run/本番の差分は「bid/ask 取得先が自分の PriceStore か、bitFlyer の get_executions か」だけ。

**Deploy 方針:** PR-1 / PR-2 / PR-3 全てマージ完了後、初めて `docker compose up` で再起動。**途中の deploy はしない**。deploy 時に同時に vegapunk スキーマを drop → create で完全リセット。

**ブランチ:** `feat/unified-trader-rewrite` (作成済み、main から派生)

**参照スペック:** 初期設計書 `docs/superpowers/specs/2026-04-14-bitflyer-live-trading-design.md` は廃止想定、本 plan が正。

---

## 0. 設計決定 (承認済み)

### データモデル

**`trading_accounts` (旧 `paper_accounts` を置換):**
```sql
CREATE TABLE trading_accounts (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    account_type TEXT NOT NULL CHECK (account_type IN ('paper', 'live')),
    exchange TEXT NOT NULL,
    strategy TEXT NOT NULL,
    initial_balance NUMERIC NOT NULL,
    current_balance NUMERIC NOT NULL,
    leverage NUMERIC NOT NULL,
    currency TEXT NOT NULL DEFAULT 'JPY',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

**`trades` (clean state):**
```sql
CREATE TABLE trades (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES trading_accounts(id),
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    exchange TEXT NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('long', 'short')),
    entry_price NUMERIC NOT NULL,        -- 実約定価格 (dry_run: bid/ask, live: API fill)
    exit_price NUMERIC,                   -- 実約定価格
    quantity NUMERIC NOT NULL,            -- 実約定数量
    leverage NUMERIC NOT NULL,
    fees NUMERIC NOT NULL DEFAULT 0,
    stop_loss NUMERIC NOT NULL,
    take_profit NUMERIC,                  -- 動的 exit 戦略は NULL
    entry_at TIMESTAMPTZ NOT NULL,
    exit_at TIMESTAMPTZ,
    pnl_amount NUMERIC,                   -- 実現損益 (決済後のみ)
    exit_reason TEXT,                     -- sl_hit / tp_hit / strategy_* / manual
    status TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    max_hold_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX trades_account_status ON trades (account_id, status);
CREATE INDEX trades_account_entry_at ON trades (account_id, entry_at);
```

**削除する旧カラム/テーブル/型:**
- trades: `child_order_acceptance_id`, `child_order_id`, `mode`, `paper_account_id` (→ `account_id`), `pnl_pips`
- TradeStatus: `Pending`, `Inconsistent` 削除、`Open` と `Closed` のみ
- OrderType: `Limit` variant + `limit()` factory + `InvalidOrderTypeError` 削除、**`Signal.order_type` フィールド自体削除** (Market 固定、将来 Limit 戦略が出たら再検討)
- migrations: `20260414000001_live_trading_support.sql` は新 migration が drop して再作成するため効果なし
- Rust 側: `assert_valid_for_mode`, `#[cfg(test)] impl Default for Trade`, `[risk] config`, `RiskConfig`, `risk_halts` table (RiskGate は PR-2 で一から再設計)

**残すテーブル:**
- `notifications` (UI ベル、カラム互換のまま)
- `strategies` / `strategy_params` (戦略メタデータ、`strategy` 外部キー制約と整合)
- `paper_account_events` (残高イベント履歴、ただし `paper_account_id` → `account_id` リネーム)
- `price_history` / `aggregated_prices` 等の既存マーケットデータテーブル (触らない)

### Signal の責務分離

```rust
// 削除: Signal.entry_price, Signal.order_type (Market 固定なので不要)
// 追加: stop_loss, take_profit を fill 確定後に Trader 側で計算
pub struct Signal {
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    pub stop_loss_pct: Decimal,           // fill 価格に対する比率 (例: 0.03 = 3%)
    pub take_profit_pct: Option<Decimal>, // 動的 exit 戦略は None
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
    pub allocation_pct: Decimal,
    pub max_hold_until: Option<DateTime<Utc>>,
}
```

4 戦略の Signal 生成は entry_price を使わず pct ベースで SL/TP を指定する。戦略内部の ATR 計算などは変わらない。

### 統一 `Trader`

```rust
// crates/executor/src/trader.rs (PaperTrader 削除、これ 1 本)
pub struct Trader {
    pool: PgPool,
    exchange: Exchange,
    account_id: Uuid,
    api: Arc<BitflyerPrivateApi>,
    price_store: Arc<PriceStore>,
    notifier: Arc<Notifier>,
    pair_configs: HashMap<String, PairConfig>,
    dry_run: bool,
}

impl OrderExecutor for Trader {
    async fn execute(&self, signal: &Signal) -> Result<Trade> { ... }
    async fn close_position(&self, id: &str, reason: ExitReason) -> Result<Trade> { ... }
    //                                              ^^^^^^^ exit_price 引数廃止
    async fn open_positions(&self) -> Result<Vec<Position>> { ... }
}
```

### bid/ask データフロー

```
bitFlyer WS (lightning_ticker)
  └─ TickerMessage { ltp, best_bid, best_ask, timestamp }
      │
      ├─→ CandleBuilder (5 分集約)
      │     └─ Candle { open, high, low, close, best_bid, best_ask, timestamp }
      │           (best_bid/ask = candle 終了時点の最新気配)
      │
      └─→ PriceStore::update(pair, ltp, best_bid, best_ask, ts)
            └─ LatestTick { ltp, best_bid, best_ask, ts }
```

戦略は `PriceEvent.candle.best_ask/bid` を参照可能。Trader は `PriceStore::latest_bid_ask(pair)` で最新気配を取得。

### overnight fee

- dry_run=true: 自前 0.04%/日 課金継続 (bitFlyer 公式と同じレート)
- dry_run=false: 自前課金スキップ、bitFlyer 側が control する (PR-2 の残高同期タスクで反映)

### Seed データ (migration 内で INSERT)

```sql
INSERT INTO trading_accounts (id, name, account_type, exchange, strategy, initial_balance, current_balance, leverage, currency) VALUES
  ('a0000000-0000-0000-0000-000000000010', '安全', 'paper', 'bitflyer_cfd', 'bb_mean_revert_v1', 30000, 30000, 2, 'JPY'),
  ('a0000000-0000-0000-0000-000000000011', '通常', 'paper', 'bitflyer_cfd', 'donchian_trend_v1', 30000, 30000, 2, 'JPY'),
  ('a0000000-0000-0000-0000-000000000012', '攻め', 'paper', 'bitflyer_cfd', 'squeeze_momentum_v1', 30000, 30000, 2, 'JPY'),
  ('a0000000-0000-0000-0000-000000000020', 'vegapunk連動', 'paper', 'bitflyer_cfd', 'donchian_trend_evolve_v1', 30000, 30000, 2, 'JPY');
```

id と name は現状踏襲 (UI の fixture 互換性は気にしないが id 変更の必要性なし)。`strategies` / `strategy_params` のシードは既存 migration 20260407000005 / 20260410000001 を参考に同等の内容を投入。

---

## PR-1: コア大改修 (3-4 日)

**目的:** 統一 Trader に向けた全ての構造変更を 1 PR でまとめる。deploy しないので intermediate broken state を気にせず一気に変えきる。

**Files (新規):**
- `migrations/20260415000001_unified_rewrite.sql` (巨大な wipe + recreate)
- `crates/executor/src/trader.rs`
- `crates/app/src/tasks/mod.rs` + `crates/app/src/tasks/execution_poller.rs` (PR-2 に pending → 不要、本 PR は該当なし)

**Files (削除):**
- `crates/executor/src/paper.rs` (Trader に統合)
- 旧 migration 20260414000001 の effect は新 migration で上書き (ファイル自体は残すが内容は無意味化)

**Files (変更):**
- `crates/core/src/types.rs` (Signal / Trade / TradeStatus / OrderType 清掃)
- `crates/core/src/executor.rs` (OrderExecutor::close_position の引数変更)
- `crates/core/src/event.rs` (PriceEvent に bid/ask)
- `crates/core/src/config.rs` ([risk] 削除、RiskConfig 削除)
- `crates/market/src/bitflyer.rs` (TickerMessage bid/ask 利用)
- `crates/market/src/candle_builder.rs` (bid/ask 渡し)
- `crates/app/src/price_store.rs` (bid/ask 保持)
- `crates/strategy/src/{bb_mean_revert,donchian_trend,donchian_trend_evolve,squeeze_momentum,swing_llm}.rs` (Signal 生成修正)
- `crates/app/src/main.rs` (dispatcher、position monitor、daily batch overnight fee 分岐)
- `crates/app/src/api/{accounts,dashboard,positions,trades,notifications,strategies}.rs` (API レスポンス型を新スキーマに追従)
- `crates/db/src/{paper_accounts,trades,notifications,strategies,summary,dashboard}.rs` (全面書き換え)
- `crates/db/Cargo.toml` (もし必要なら)
- `dashboard-ui/src/api/types.ts` (TypeScript 型追従)
- `dashboard-ui/src/pages/{Accounts,Overview,Positions,Trades,Analysis,Strategies}.tsx` (最低限動く状態)
- `config/default.toml` ([risk] 削除)

### Task 1: DB 大改修 migration

- [ ] **Step 1**: `migrations/20260415000001_unified_rewrite.sql` を作成

```sql
-- Unified trader rewrite — wipe all stateful tables and rebuild with clean schema.
-- 注意: 既存の paper trade データは全消失する。deploy 前の最終段階で
-- 実行される想定 (本 migration 適用 = 旧データ破棄の合意と同値)。

BEGIN;

-- 1) drop old tables and types
DROP TABLE IF EXISTS paper_account_events CASCADE;
DROP TABLE IF EXISTS trades CASCADE;
DROP TABLE IF EXISTS paper_accounts CASCADE;
DROP TABLE IF EXISTS notifications CASCADE;
DROP TABLE IF EXISTS strategy_params CASCADE;
DROP TABLE IF EXISTS strategies CASCADE;
DROP TABLE IF EXISTS risk_halts CASCADE;  -- PR #38 で作ったが未使用
-- 旧 CHECK 制約 / partial unique index は TABLE drop で一緒に消える

-- 2) strategies (戦略メタ)
CREATE TABLE strategies (
    name TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    category TEXT NOT NULL,
    risk_level TEXT NOT NULL,
    description TEXT,
    algorithm TEXT,
    default_params JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 3) strategy_params (戦略別パラメータ、vegapunk 学習ループ向け)
CREATE TABLE strategy_params (
    strategy_name TEXT PRIMARY KEY REFERENCES strategies(name),
    params JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 4) trading_accounts (paper_accounts 置換)
CREATE TABLE trading_accounts (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    account_type TEXT NOT NULL CHECK (account_type IN ('paper', 'live')),
    exchange TEXT NOT NULL,
    strategy TEXT NOT NULL REFERENCES strategies(name),
    initial_balance NUMERIC NOT NULL CHECK (initial_balance >= 0),
    current_balance NUMERIC NOT NULL,
    leverage NUMERIC NOT NULL CHECK (leverage >= 1),
    currency TEXT NOT NULL DEFAULT 'JPY',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 5) trades (clean)
CREATE TABLE trades (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES trading_accounts(id) ON DELETE RESTRICT,
    strategy_name TEXT NOT NULL REFERENCES strategies(name),
    pair TEXT NOT NULL,
    exchange TEXT NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('long', 'short')),
    entry_price NUMERIC NOT NULL,
    exit_price NUMERIC,
    quantity NUMERIC NOT NULL CHECK (quantity > 0),
    leverage NUMERIC NOT NULL,
    fees NUMERIC NOT NULL DEFAULT 0,
    stop_loss NUMERIC NOT NULL,
    take_profit NUMERIC,
    entry_at TIMESTAMPTZ NOT NULL,
    exit_at TIMESTAMPTZ,
    pnl_amount NUMERIC,
    exit_reason TEXT,
    status TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    max_hold_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX trades_account_status ON trades (account_id, status);
CREATE INDEX trades_account_entry_at ON trades (account_id, entry_at DESC);

-- 6) paper_account_events (残高履歴、カラム名追従)
CREATE TABLE account_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id UUID NOT NULL REFERENCES trading_accounts(id) ON DELETE RESTRICT,
    trade_id UUID REFERENCES trades(id),
    event_type TEXT NOT NULL CHECK (event_type IN ('margin_lock', 'margin_release', 'trade_open', 'trade_close', 'overnight_fee', 'balance_sync')),
    amount NUMERIC NOT NULL,
    balance_after NUMERIC NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    metadata JSONB
);
CREATE INDEX account_events_account_time ON account_events (account_id, occurred_at DESC);

-- 7) notifications (UI ベル、クリーン再作成)
CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL,
    account_id UUID REFERENCES trading_accounts(id),
    trade_id UUID REFERENCES trades(id),
    strategy_name TEXT,
    pair TEXT,
    direction TEXT,
    price NUMERIC,
    pnl_amount NUMERIC,
    exit_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    read_at TIMESTAMPTZ
);
CREATE INDEX notifications_unread ON notifications (created_at DESC) WHERE read_at IS NULL;

-- 8) strategies seed (旧 migration 20260407000005 / 20260410000001 の内容と等価)
INSERT INTO strategies (name, display_name, category, risk_level, description, algorithm, default_params) VALUES
  ('bb_mean_revert_v1', '慎重 (low risk)', 'crypto', 'low',
   'ボリンジャーバンド下抜け/上抜け後の平均回帰を狙う。BB ± 2.5σ / RSI 14 / ATR 14。24h タイムリミット。',
   'Bollinger Bands + RSI mean reversion',
   '{"bb_period":20,"bb_stddev":2.5,"rsi_period":14,"atr_period":14,"sl_max_pct":0.02,"time_limit_hours":24}'::jsonb),
  ('donchian_trend_v1', '標準ブレイクアウト v1 (Donchian)', 'crypto', 'medium',
   '20 本ブレイクアウト + 10 本トレーリング。ATR で SL 固定。Turtle System 系。',
   'Donchian channel breakout with trailing exit',
   '{"entry_channel":20,"exit_channel":10,"atr_period":14,"atr_baseline_bars":50}'::jsonb),
  ('squeeze_momentum_v1', '攻め (high risk)', 'crypto', 'high',
   'BB squeeze (KC 内収束) + ブレイクアウト + EMA トレーリング。48h タイムリミット。',
   'Squeeze momentum + EMA trailing',
   '{"bb_period":20,"kc_period":20,"atr_period":14,"ema_trail_period":21,"squeeze_bars":6}'::jsonb),
  ('donchian_trend_evolve_v1', 'ブレイクアウト進化版 (Donchian Evolve)', 'crypto', 'medium',
   'donchian_trend_v1 ベース。Vegapunk 学習ループでパラメータを週次自動更新。baseline (通常) との A/B.',
   '(donchian_trend_v1 と同一アルゴリズム。パラメータのみ可変。)',
   '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb);

-- 9) strategy_params seed (evolve 用、initial は baseline と同じ)
INSERT INTO strategy_params (strategy_name, params) VALUES
  ('donchian_trend_evolve_v1',
   '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb);

-- 10) trading_accounts seed (paper 4 アカウント)
INSERT INTO trading_accounts (id, name, account_type, exchange, strategy, initial_balance, current_balance, leverage, currency) VALUES
  ('a0000000-0000-0000-0000-000000000010', '安全', 'paper', 'bitflyer_cfd', 'bb_mean_revert_v1', 30000, 30000, 2, 'JPY'),
  ('a0000000-0000-0000-0000-000000000011', '通常', 'paper', 'bitflyer_cfd', 'donchian_trend_v1', 30000, 30000, 2, 'JPY'),
  ('a0000000-0000-0000-0000-000000000012', '攻め', 'paper', 'bitflyer_cfd', 'squeeze_momentum_v1', 30000, 30000, 2, 'JPY'),
  ('a0000000-0000-0000-0000-000000000020', 'vegapunk連動', 'paper', 'bitflyer_cfd', 'donchian_trend_evolve_v1', 30000, 30000, 2, 'JPY');

COMMIT;
```

- [ ] **Step 2**: ローカル DB で apply 確認

```bash
docker compose up -d db
docker compose exec -T db psql -U auto-trader -d auto_trader \
    -f /dev/stdin < migrations/20260415000001_unified_rewrite.sql 2>&1 | tail -20
docker compose exec -T db psql -U auto-trader -d auto_trader -c "\dt"
docker compose exec -T db psql -U auto-trader -d auto_trader -c "SELECT name, account_type, strategy FROM trading_accounts;"
```

Expected: 7 テーブル存在 (strategies / strategy_params / trading_accounts / trades / account_events / notifications + 既存 price 系)、4 paper アカウントが見える。

- [ ] **Step 3**: 前 migration 20260414000001 を**残す** (履歴として)、ただし新 migration が drop するので効果は消える。コメントで「This migration is superseded by 20260415000001 unified_rewrite」と注記。

- [ ] **Step 4**: コミット

```bash
git add migrations/
git commit -m "$(cat <<'EOF'
feat(db): unified rewrite — wipe old schema, rebuild clean

Drops all paper-era state tables and rebuilds with:
- trading_accounts (replaces paper_accounts, account_type='paper'|'live')
- trades (clean: removes child_order_*, mode, pending/inconsistent)
- account_events (renamed from paper_account_events)
- strategies / strategy_params (re-seeded identical to prior content)
- notifications (fresh)

Seeds 4 paper accounts with their original ids/names so the user
doesn't re-register. All past paper trade history is discarded
— it was computed against a broken fill model (signal.entry_price
instead of bid/ask) and is not worth preserving.

Applied manually during deploy after all three PRs in this series
land; no running server should hit this migration.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 2: `Signal` / `OrderType` / `TradeStatus` / `Trade` の型清掃

- [ ] **Step 1**: `crates/core/src/types.rs` を大改修

削除:
- `OrderType::Limit` variant
- `impl OrderType { pub fn limit(...) }`
- `InvalidOrderTypeError`
- `Signal.order_type` フィールド (Market 固定、将来 Limit 戦略が出たら別 enum で検討)
- `Signal.entry_price`, `Signal.stop_loss`, `Signal.take_profit` (絶対値) フィールド
- `TradeStatus::Pending`, `TradeStatus::Inconsistent` バリアント
- `TradeStatus::assert_valid_for_mode` メソッド
- `TradeStatus::as_str` に `pending`/`inconsistent` 分岐削除
- `Trade.child_order_acceptance_id`, `Trade.child_order_id` フィールド
- `Trade.mode` フィールド (account_id から account_type を引けば分かる)
- `Trade.pnl_pips` フィールド
- `#[cfg(test)] impl Default for Trade`
- `crates/core/Cargo.toml` の `[features] testing` 削除

追加 (Signal):
```rust
pub struct Signal {
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    pub stop_loss_pct: Decimal,
    pub take_profit_pct: Option<Decimal>,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
    #[serde(default = "default_allocation_pct")]
    pub allocation_pct: Decimal,
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
}
```

変更 (Trade):
```rust
pub struct Trade {
    pub id: Uuid,
    pub account_id: Uuid,                    // 旧 paper_account_id
    pub strategy_name: String,
    pub pair: Pair,
    pub exchange: Exchange,
    pub direction: Direction,
    pub entry_price: Decimal,                // 実約定
    pub exit_price: Option<Decimal>,         // 実約定
    pub stop_loss: Decimal,
    pub take_profit: Option<Decimal>,        // Option に変更 (動的 exit 戦略は None)
    pub quantity: Decimal,                   // Option から Decimal に (全 trade で quantity 必須)
    pub leverage: Decimal,
    pub fees: Decimal,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<ExitReason>,
    pub status: TradeStatus,                 // Open / Closed のみ
    pub max_hold_until: Option<DateTime<Utc>>,
}
```

- [ ] **Step 2**: `crates/core/src/executor.rs` を変更

```rust
pub trait OrderExecutor: Send + Sync + 'static {
    fn execute(&self, signal: &Signal) -> impl Future<Output = Result<Trade>> + Send;
    fn open_positions(&self) -> impl Future<Output = Result<Vec<Position>>> + Send;
    fn close_position(
        &self,
        id: &str,
        exit_reason: ExitReason,
        // exit_price 引数を廃止 — Trader が自分で fill 決定
    ) -> impl Future<Output = Result<Trade>> + Send;
}
```

- [ ] **Step 3**: 既存テスト/呼び出し箇所のコンパイルエラーを洗い出し

```bash
cargo check --workspace 2>&1 | grep 'error' | head -30
```

- [ ] **Step 4**: コミット (コンパイルは通らないが型の核を固定する commit、次の Task で残りを追従)

```bash
git commit -m "refactor(core): unify Signal/Trade/TradeStatus for dry_run-based trader

Signal loses entry_price/stop_loss/take_profit absolute values — it
declares intent only (direction, SL/TP as pct of fill). Trade loses
mode/pending/inconsistent/child_order_*/pnl_pips — account_id links
to trading_accounts which carries account_type. TradeStatus is back
to Open|Closed. OrderExecutor::close_position no longer takes an
explicit exit_price; the Trader decides the fill itself.

Rest of the workspace is broken after this commit and fixed in the
immediately following commits.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

### Task 3: `PriceEvent` / `Candle` に bid/ask、WebSocket 経路整備

- [ ] **Step 1**: `crates/core/src/types.rs` の `Candle` に追加

```rust
pub struct Candle {
    pub pair: Pair,
    pub exchange: Exchange,
    pub timeframe: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Option<u64>,
    pub best_bid: Option<Decimal>,   // candle 終了時点の最新気配
    pub best_ask: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
}
```

`Option` にする理由: OANDA 等の bid/ask 非対応データソースでも Candle 型を共有。bid/ask 不明時は `None`、dry_run Trader は `None` を受けたら `close` フォールバック + warn ログ (本番環境の bitFlyer は必ず Some で埋まる設計)。

- [ ] **Step 2**: `crates/market/src/bitflyer.rs` の `TickerMessage` の `#[allow(dead_code)]` 削除、CandleBuilder に bid/ask 引数追加

- [ ] **Step 3**: `crates/market/src/candle_builder.rs` を bid/ask 保持対応に

```rust
impl CandleBuilder {
    pub fn on_tick(
        &mut self,
        price: Decimal,
        size: Decimal,
        ts: DateTime<Utc>,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
    ) -> Option<Candle> {
        // ... 既存ロジック
        // 完成した candle に self.last_best_bid / self.last_best_ask を埋める
    }
}
```

- [ ] **Step 4**: `crates/app/src/price_store.rs` を bid/ask 保持対応

```rust
pub struct LatestTick {
    pub ltp: Decimal,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub ts: DateTime<Utc>,
}

impl PriceStore {
    pub fn latest_bid_ask(&self, pair: &Pair) -> Option<(Decimal, Decimal)>;
    pub fn update(&self, pair: &Pair, ltp: Decimal, bid: Option<Decimal>, ask: Option<Decimal>, ts: DateTime<Utc>);
}
```

- [ ] **Step 5**: Candle/PriceEvent deserialize テストに bid/ask フィールド追加、既存テストの Candle 構築箇所を全修正

- [ ] **Step 6**: コミット

```bash
git commit -m "feat(market,core): thread best_bid/best_ask through Candle/PriceEvent/PriceStore

bitFlyer ticker already provides best_bid/best_ask but we were
throwing them away. Now they flow end-to-end to PriceStore and
into every Candle. OANDA doesn't provide them, so the fields are
Option — consumers that need bid/ask (dry_run Trader) fall back to
close with a warn log when None.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

### Task 4: 4 戦略の Signal 生成修正

- [ ] **Step 1**: 各戦略 (`bb_mean_revert.rs`, `donchian_trend.rs`, `donchian_trend_evolve.rs`, `squeeze_momentum.rs`) で `Signal { entry_price, stop_loss, take_profit, .. }` を `Signal { stop_loss_pct, take_profit_pct, .. }` に書き換え

- [ ] **Step 2**: 各戦略の SL/TP 計算を絶対値 → 比率に変更

```rust
// 現状: sl_offset = atr × 1.5、stop_loss = entry - sl_offset
// 新: sl_offset = atr × 1.5、stop_loss_pct = sl_offset / entry
let sl_pct = (atr * dec!(1.5)) / entry;
Signal { stop_loss_pct: sl_pct, take_profit_pct: None, ... }
```

「park された TP」(entry × 1000) は **削除** (take_profit_pct = None で表現)。

- [ ] **Step 3**: swing_llm の Signal 生成も同様に修正 (FX 用、現状 enabled=false なので動かないが型整合性のため)

- [ ] **Step 4**: 各戦略の既存テスト修正 (Signal リテラル、Trade 構築箇所)

- [ ] **Step 5**: `cargo test -p auto-trader-strategy` で全緑

- [ ] **Step 6**: コミット

```bash
git commit -m "feat(strategy): emit Signal with stop_loss_pct instead of absolute prices

Strategies now declare intent only — direction and SL/TP expressed
as percentages of the eventual fill price. The 'parked' take_profit
(entry × 1000 or entry / 1000 sentinel values) is gone; dynamic
exit strategies set take_profit_pct = None and are closed via
on_open_positions hooks as before. The four crypto strategies and
swing_llm are all updated.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

### Task 5: 統一 `Trader` 実装

- [ ] **Step 1**: `crates/executor/src/paper.rs` を削除

- [ ] **Step 2**: `crates/executor/src/trader.rs` を新規作成

```rust
//! Unified trader — serves both paper and live accounts.
//!
//! The only difference between paper and live:
//!   dry_run == true  → fill price from local PriceStore (bid/ask)
//!                      no bitFlyer API call
//!   dry_run == false → fill price from bitFlyer get_executions
//!                      actual order placed via send_child_order
//!
//! Everything else — DB writes, balance management, margin lock,
//! overnight fees, pnl computation, notifications — is identical.

pub struct Trader {
    pool: PgPool,
    exchange: Exchange,
    account_id: Uuid,
    api: Arc<BitflyerPrivateApi>,
    price_store: Arc<PriceStore>,
    notifier: Arc<Notifier>,
    pair_configs: HashMap<String, PairConfig>,
    dry_run: bool,
}

impl Trader {
    pub fn new(...) -> Self { ... }
    
    async fn fill_open(&self, signal: &Signal, quantity: Decimal) -> Result<(Decimal, Decimal)> {
        if self.dry_run {
            let (bid, ask) = self.price_store.latest_bid_ask(&signal.pair)
                .ok_or_else(|| anyhow::anyhow!("no bid/ask for {}", signal.pair))?;
            let price = match signal.direction {
                Direction::Long => ask,
                Direction::Short => bid,
            };
            Ok((price, quantity))
        } else {
            let req = signal_to_send_child_order(signal, quantity);
            let resp = self.api.send_child_order(req).await?;
            self.poll_executions(&resp.child_order_acceptance_id, Duration::from_secs(5)).await
        }
    }
    
    async fn fill_close(&self, trade: &Trade) -> Result<Decimal> {
        if self.dry_run {
            let (bid, ask) = self.price_store.latest_bid_ask(&trade.pair)?;
            Ok(match trade.direction {
                Direction::Long => bid,   // long decurring = sell
                Direction::Short => ask,
            })
        } else {
            let req = opposite_side_market_order(trade);
            let resp = self.api.send_child_order(req).await?;
            let (price, _qty) = self.poll_executions(&resp.child_order_acceptance_id, Duration::from_secs(5)).await?;
            Ok(price)
        }
    }
    
    async fn poll_executions(&self, acceptance_id: &str, timeout: Duration) -> Result<(Decimal, Decimal)> {
        // 1 秒間隔で最大 timeout まで get_executions をポーリング
        // 約定したら (weighted_avg_price, total_size) を返す
        // timeout したら Err (trade は DB に入れない)
    }
}

impl OrderExecutor for Trader {
    async fn execute(&self, signal: &Signal) -> Result<Trade> {
        // 1. position sizing (既存 PositionSizer 再利用)
        let balance = db::get_account_balance(self.account_id).await?;
        let sizer = PositionSizer::new(self.leverage_for_account()?, self.pair_configs.clone());
        let quantity = sizer.calculate_quantity(balance, signal.allocation_pct, /* hint price */ self.price_store.latest_bid_ask(&signal.pair)?.1)?;
        
        // 2. fill 確定
        let (fill_price, actual_qty) = self.fill_open(signal, quantity).await?;
        
        // 3. SL/TP を fill_price から逆算
        let stop_loss = match signal.direction {
            Direction::Long => fill_price * (Decimal::ONE - signal.stop_loss_pct),
            Direction::Short => fill_price * (Decimal::ONE + signal.stop_loss_pct),
        };
        let take_profit = signal.take_profit_pct.map(|pct| match signal.direction {
            Direction::Long => fill_price * (Decimal::ONE + pct),
            Direction::Short => fill_price * (Decimal::ONE - pct),
        });
        
        // 4. Trade 構築 + DB 記録 (margin lock トランザクション含む、既存 PaperTrader と同様)
        let trade = Trade { id: Uuid::new_v4(), account_id: self.account_id, ..., entry_price: fill_price, quantity: actual_qty, stop_loss, take_profit, status: Open, ... };
        
        let mut tx = self.pool.begin().await?;
        db::insert_trade(&mut tx, &trade).await?;
        db::lock_margin(&mut tx, self.account_id, trade.id, fill_price * actual_qty / trade.leverage).await?;
        db::insert_notification_trade_opened(&mut tx, &trade).await?;
        tx.commit().await?;
        
        // 5. Slack 通知 (fire-and-forget)
        tokio::spawn(...);
        
        Ok(trade)
    }
    
    async fn close_position(&self, id: &str, reason: ExitReason) -> Result<Trade> {
        // 1. trade lock + open 確認 (既存 PaperTrader の SELECT FOR UPDATE パターン)
        // 2. fill_close() で実 exit 価格取得
        // 3. pnl = (exit_price - entry_price) × qty (long) or 逆 (short)
        // 4. update trade (status=closed, exit_price, exit_at, pnl, exit_reason, fees)
        // 5. margin release + balance 更新 + notification insert (1 tx)
        // 6. Slack 通知
    }
    
    async fn open_positions(&self) -> Result<Vec<Position>> {
        let trades = db::trades::get_open_trades_by_account(&self.pool, self.account_id).await?;
        Ok(trades.into_iter().map(|t| Position { trade: t }).collect())
    }
}
```

- [ ] **Step 2**: `signal_mapping` は Trader の内部 helper として `trader.rs` に統合 (別ファイル不要、YAGNI)

- [ ] **Step 3**: `crates/executor/src/lib.rs` 更新

```rust
pub mod position_sizer;
pub mod trader;
```

- [ ] **Step 4**: `crates/executor/Cargo.toml` に `auto-trader-market` (BitflyerPrivateApi) / `auto-trader-notify` を依存追加

- [ ] **Step 5**: `crates/executor/tests/trader_test.rs` 新規作成

統合テスト 6-8 本:
- `dry_run_execute_uses_ask_for_long_and_bid_for_short`
- `live_execute_calls_send_child_order_and_records_actual_fill`
- `live_execute_timeout_returns_err_and_no_trade_inserted`
- `dry_run_close_uses_bid_for_long_exit`
- `live_close_places_opposite_market_and_records_actual_fill`
- `execute_respects_margin_lock_transaction` (trade insert と margin lock が同 tx)

- [ ] **Step 6**: コミット

```bash
git commit -m "feat(executor): unified Trader (replaces PaperTrader)

Single OrderExecutor implementation serves both paper and live
accounts. Dry-run takes fill price from PriceStore (bid for Short
close / Long entry on Sell side? — see fill_close); live goes
through BitflyerPrivateApi with 5s get_executions polling.

Everything else — margin lock transaction, overnight fee accrual,
pnl computation, balance update, notification insert — runs the
same code path regardless of dry_run. This is the structural
guarantee that 'paper = live except for the API call'.

PaperTrader is deleted; no migration for callers because the
OrderExecutor trait shape didn't change (except close_position's
removed exit_price param which no strategy set meaningfully).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

### Task 6: DB 層追従、API レスポンス型追従、main.rs dispatcher

- [ ] **Step 1**: `crates/db/src/paper_accounts.rs` → `trading_accounts.rs` にリネーム、関数名も `get_paper_account` → `get_trading_account` 等

- [ ] **Step 2**: `crates/db/src/trades.rs` を新スキーマ対応:
  - カラム追従 (`paper_account_id` → `account_id`、`mode` 削除、`pnl_pips` 削除、`child_order_*` 削除、`quantity` を Decimal に)
  - 関数シグネチャ調整
  - `insert_trade_with_executor` / `update_trade_closed` / `get_open_trades*` 等を順次
  - PR 1/PR 3 で入れた live 関連関数 (`list_pending_live`, `promote_pending_to_open`, `mark_inconsistent`) は**削除**

- [ ] **Step 3**: `crates/db/src/notifications.rs` を新スキーマ対応

- [ ] **Step 4**: `crates/app/src/api/*.rs` 全般を新型に追従
  - `accounts.rs`: `paper_accounts` → `trading_accounts`、`account_type` を含める
  - `positions.rs`: `paper_account_id` → `account_id`、`mode` を削除、`take_profit: Option<Decimal>` のまま
  - `trades.rs`: 同様
  - `dashboard.rs`: 内部 JOIN 対象変更

- [ ] **Step 5**: `crates/app/src/main.rs` の dispatcher 書き換え

```rust
// account_type で Trader を構築するだけ、dry_run フラグに置き換え
let accounts = db::trading_accounts::list_all(&pool).await?;
for account in accounts {
    let dry_run = match account.account_type {
        AccountType::Paper => true,
        AccountType::Live => false,
    };
    let trader = Trader::new(
        pool.clone(),
        account.exchange,
        account.id,
        bitflyer_api.clone(),
        price_store.clone(),
        notifier.clone(),
        pair_configs.clone(),
        dry_run,
    );
    // trader を signal executor タスクに渡す
}
```

- [ ] **Step 6**: `main.rs` の overnight fee バッチを account_type で分岐

```rust
// dry_run=true のアカウントのみ自前で 0.04%/日 課金
// dry_run=false は bitFlyer が引く → skip
for account in paper_accounts_only() {
    apply_overnight_fee(account, 0.04 / 100);
}
```

- [ ] **Step 7**: `main.rs` の position monitor を `exit_price` 引数なしに追従

```rust
// 旧: trader.close_position(id, reason, exit_price)
// 新: trader.close_position(id, reason)
```

- [ ] **Step 8**: `config/default.toml` の `[risk]` セクション削除、`[live]` セクションは残す (enabled/dry_run flag は PR-2 で意味を持たせる、本 PR では未使用)

- [ ] **Step 9**: `crates/app/tests/*.rs` の統合テストを追従

- [ ] **Step 10**: コミット

```bash
git commit -m "refactor: db/api/main dispatcher follow unified schema + Trader

- crates/db: paper_accounts→trading_accounts rename, trade columns
  aligned (account_id, no mode/child_order_*/pnl_pips)
- crates/app/api: REST responses drop paper_account_id and expose
  account_id + account_type for UI
- crates/app/main: dispatcher constructs one Trader per account
  with dry_run derived from account_type. Overnight fee batch now
  skips live accounts (bitFlyer accrues the swap itself).
- position monitor + exit executor call close_position(id, reason)
  without an explicit exit_price — the Trader fills from bid/ask
  or the live API.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

### Task 7: Dashboard UI 追従 (最低限動く状態)

- [ ] **Step 1**: `dashboard-ui/src/api/types.ts` を新スキーマに追従
  - `PaperAccount` → `TradingAccount` リネーム
  - `paper_account_id` → `account_id`
  - `account_type: 'paper' | 'live'` 追加
  - `Trade` から `mode` / `pnl_pips` / `child_order_*` 削除
  - `take_profit: string | null` そのまま

- [ ] **Step 2**: Accounts 画面: `/api/paper-accounts` → `/api/trading-accounts` にエンドポイント追従、account_type 列を (目立たず) 追加表示

- [ ] **Step 3**: Positions / Trades / Overview: フィールド名追従

- [ ] **Step 4**: **見た目の改善 (live バッジ等) は PR-3 で対応**。本 Task は API が 200 返すレベル維持が目標。

- [ ] **Step 5**: `npm run build` 成功確認

- [ ] **Step 6**: コミット

```bash
git commit -m "ui: follow unified schema (api shape only; visual polish in PR-3)

TypeScript types and API paths catch up to trading_accounts /
account_id / account_type. No visible layout changes — live
account visual treatment (red badge, filter) lands in PR-3.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

### Task 8: 最終検証 + code-review + PR 作成

- [ ] **Step 1**: `cargo test --workspace` 全緑、`cargo clippy --workspace --all-targets -- -D warnings` 警告ゼロ、`cargo fmt --all -- --check` 差分ゼロ

- [ ] **Step 2**: Docker build (**up しない**、build のみ)

```bash
docker compose build auto-trader 2>&1 | tail -5
```

Expected: ビルド成功。

- [ ] **Step 3**: `simplify` スキル

- [ ] **Step 4**: `code-review` スキル (codex レビュー)

- [ ] **Step 5**: `superpowers:finishing-a-development-branch` で push + PR

**PR 本文**: 本 plan の要約 + 「Safety (PR-2) と UI polish (PR-3) 完了後に初めて deploy する、途中 deploy 禁止」を明記

---

## PR-2: 安全装置 (1-2 日)

**目的:** 本番投入前に必須の安全機構。RiskGate / 起動時ポジション突合 / 残高同期。

### Task 1: RiskGate

`crates/executor/src/risk_gate.rs` 新規:

```rust
pub enum GateDecision { Pass, Reject(RejectReason) }
pub enum RejectReason {
    KillSwitchActive { until: DateTime<Utc> },
    PriceTickStale { age_secs: u64 },
    DuplicatePosition { existing_trade_id: Uuid },
    DailyLossLimitExceeded { loss: Decimal, limit: Decimal },
}

pub struct RiskGate {
    pool: PgPool,
    price_store: Arc<PriceStore>,
    notifier: Arc<Notifier>,
    daily_loss_limit_pct: Decimal,
    price_freshness_secs: u64,
}

impl RiskGate {
    pub async fn check(&self, signal: &Signal, account: &TradingAccount) -> GateDecision;
}
```

新 migration: `risk_halts` テーブルを再導入 (PR #38 で作ったものを新設計で再作成)。Signal executor タスクの先頭で RiskGate::check を呼び、Pass のみ Trader に流す。

### Task 2: 起動時ポジション突合 (live のみ)

`crates/app/src/tasks/startup_reconciler.rs`:

```rust
pub async fn reconcile_on_startup(pool, api, notifier) -> Result<()> {
    // 1. db::trades::get_open_trades_by_account (account_type='live' のみ)
    // 2. api.get_positions("FX_BTC_JPY")
    // 3. 差分検出: DB だけにあるもの / 取引所だけにあるもの
    // 4. Slack 通知 (StartupReconciliationDiff) + 手動対処要
}
```

### Task 3: 残高同期タスク (live のみ)

`crates/app/src/tasks/balance_sync.rs`:

```rust
pub struct BalanceSyncTask { ... }
impl BalanceSyncTask {
    pub async fn tick(&self) { /* getcollateral → db update_balance */ }
    pub async fn run_forever(self) { /* 5 分間隔 */ }
}
```

### Task 4: main.rs 配線 + テスト + PR

---

## PR-3: UI polish (0.5 日)

**目的:** live account の視覚的区別、動的 exit 戦略の TP 表示整理、account_type フィルタ。

- `dashboard-ui/src/pages/Accounts.tsx`: `account_type === 'live'` を赤枠 + "LIVE" バッジ
- `dashboard-ui/src/pages/Positions.tsx`: live ポジションを視認しやすく
- `dashboard-ui/src/components/PageFilters.tsx`: `account_type` フィルタ追加
- `dashboard-ui/src/pages/Overview.tsx`: live/paper 集計分離

**テスト**: `npm run build` 成功 + 目視 (ローカル dev server)

**PR 作成**: code-review → push → Copilot 対応

---

## デプロイ手順 (3 PR 全マージ後)

1. `main` を pull
2. Docker で新 image build

```bash
docker compose build --no-cache auto-trader
```

3. DB マイグレーション手動適用 + vegapunk reset

```bash
# 既存 DB wipe (必要ならダンプを取っておく)
docker compose exec -T db psql -U auto-trader -d auto_trader \
  -f /dev/stdin < migrations/20260415000001_unified_rewrite.sql

# vegapunk schema reset (vegapunk サービスは別インスタンス、手順は vegapunk 側 README 参照)
# 仮のコマンド:
docker compose exec vegapunk /app/scripts/reset-schema.sh fx-trading

# 確認
docker compose exec -T db psql -U auto-trader -d auto_trader -c "\dt"
docker compose exec -T db psql -U auto-trader -d auto_trader -c "SELECT name, account_type FROM trading_accounts;"
```

4. auto-trader up

```bash
docker compose up -d auto-trader
docker compose logs --tail=30 auto-trader
```

Expected: 4 paper アカウント (安全/通常/攻め/vegapunk連動) 登録済み、戦略 4 本登録済み、bitFlyer WebSocket 接続、API listening on 3001。**live アカウントはまだ存在しない** (手動で INSERT するか UI から追加するかは運用判断)。

5. ペーパーで 1 週間以上走らせて真の期待値観測

6. 戦略が利益出ると確認できたら live 口座作成 (bitFlyer API キー発行 + `trading_accounts` に `account_type='live'` 1 レコード追加)

---

## 後回し (本 3 PR スコープ外、将来別 PR)

- OANDA (FX) の live 対応 (現状 OANDA は disabled、bitFlyer のみ focus)
- Trade 検索用の追加インデックス
- dashboard UI の細かなデザイン刷新
- 手動緊急停止 CLI コマンド
- オペレーション runbook ドキュメント (`specs/live-operations.md`)
