# PR 1: Live Trading Foundation (Signal OrderType / Notifier / DB schema)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** bitFlyer ライブトレード実装の土台を整える。既存のペーパートレード挙動は一切変えず、後続 PR が乗るための型 / 設定 / DB スキーマ / 通知 crate を追加する。

**Architecture:** `Signal` に `order_type` を追加し戦略側が成行/指値を指定できるようにする。`TradeStatus` に `pending` / `inconsistent` を追加。`trades` テーブルに bitFlyer 注文 ID 列を追加し、`pending/open` が同一 `account × strategy × pair` で同時に 1 件しか存在しないよう partial unique index を張る。外部通知用の `notify` crate を新設し、Slack Webhook 送信を実装。`.env.example` と `config/default.toml` に live / risk 関連の設定を追加。

**Tech Stack:** Rust (workspace edition 2024), sqlx + PostgreSQL, reqwest (既存), serde, tokio, rust_decimal。テストは cargo test + wiremock (Slack 疎通検証用)。

**ブランチ:** `feat/live-trading-foundation` (既に作成済み)

**参照スペック:** `docs/superpowers/specs/2026-04-14-bitflyer-live-trading-design.md`

---

## 0. Scope と非スコープ

**本 PR で実装する:**
- `OrderType` enum 型 + `Signal.order_type` フィールド
- `TradeStatus::Pending` / `TradeStatus::Inconsistent`
- `Trade.child_order_acceptance_id` / `Trade.child_order_id`
- 既存4戦略 (bb_mean_revert / donchian_trend / donchian_trend_evolve / squeeze_momentum) を `OrderType::Market` で Signal 生成するよう修正
- DB マイグレーション: trade_status 拡張、trades 列追加、partial unique index、risk_halts テーブル
- 新 crate `crates/notify` + Slack Webhook 送信
- `.env.example`、`config/default.toml`、`BitflyerConfig` への秘匿設定追加
- `RiskConfig` / `LiveConfig` 構造体追加

**本 PR で実装しない:**
- `BitflyerPrivateApi`（PR 2）
- `LiveTrader` / `RiskGate`（PR 3, 4）
- `ExecutionPollingTask` / `ReconcilerTask`（PR 3, 5）
- main.rs の executor dispatcher 配線（PR 6）

---

## File Structure

**新規作成:**
- `crates/notify/Cargo.toml` — 新 crate
- `crates/notify/src/lib.rs` — `Notifier` + `NotifyEvent` + Slack Webhook 実装
- `crates/notify/tests/slack_integration_test.rs` — wiremock で Slack Webhook 疎通検証
- `migrations/20260414000001_live_trading_support.sql` — スキーマ拡張

**変更:**
- `Cargo.toml` (workspace) — `crates/notify` を members に追加、`hmac` / `sha2` / `hex` / `wiremock` を workspace deps に予約（PR 2 で使用）
- `crates/core/src/types.rs` — `OrderType` enum、`Signal.order_type`、`TradeStatus::Pending/Inconsistent`、`Trade.child_order_acceptance_id/child_order_id`
- `crates/core/src/config.rs` — `BitflyerConfig` に `api_key` / `api_secret` 追加、`RiskConfig` / `LiveConfig` 新設、`AppConfig` に `risk` / `live` 追加
- `crates/core/Cargo.toml` — 変更なし（serde default が効く）
- `crates/strategy/src/bb_mean_revert.rs` — Signal 生成に `order_type: OrderType::Market`
- `crates/strategy/src/donchian_trend.rs` — 同上
- `crates/strategy/src/donchian_trend_evolve.rs` — 同上
- `crates/strategy/src/squeeze_momentum.rs` — 同上
- `crates/strategy/src/swing_llm.rs` — 同上（FX だが既存コードに Signal 生成があるため）
- `crates/db/src/trades.rs` — `pending` / `inconsistent` を TradeStatus deserialize に対応、`child_order_acceptance_id` / `child_order_id` を insert/select 対応
- `.env.example` — BITFLYER_API_KEY / BITFLYER_API_SECRET / SLACK_WEBHOOK_URL / LIVE_DRY_RUN 追加
- `config/default.toml` — `[risk]` / `[live]` セクション追加

---

## Task 1: `OrderType` enum を追加する

**Files:**
- Modify: `crates/core/src/types.rs`

- [ ] **Step 1: 失敗するテストを書く**

`crates/core/src/types.rs` の `mod tests` ブロック末尾（`}` の直前）に以下を追加：

```rust
    #[test]
    fn order_type_serializes_market() {
        let json = serde_json::to_string(&OrderType::Market).unwrap();
        assert_eq!(json, r#"{"type":"market"}"#);
    }

    #[test]
    fn order_type_serializes_limit_with_price() {
        let ot = OrderType::Limit { price: dec!(100.5) };
        let json = serde_json::to_string(&ot).unwrap();
        assert_eq!(json, r#"{"type":"limit","price":"100.5"}"#);
    }

    #[test]
    fn order_type_roundtrip_market() {
        let ot = OrderType::Market;
        let json = serde_json::to_string(&ot).unwrap();
        let back: OrderType = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, OrderType::Market));
    }

    #[test]
    fn order_type_roundtrip_limit() {
        let ot = OrderType::Limit { price: dec!(150.25) };
        let json = serde_json::to_string(&ot).unwrap();
        let back: OrderType = serde_json::from_str(&json).unwrap();
        match back {
            OrderType::Limit { price } => assert_eq!(price, dec!(150.25)),
            _ => panic!("expected Limit"),
        }
    }
```

- [ ] **Step 2: テストを実行して失敗を確認**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
cargo test -p auto-trader-core order_type 2>&1 | tail -20
```

Expected: コンパイルエラー (`cannot find type 'OrderType'`)

- [ ] **Step 3: `OrderType` enum を実装**

`crates/core/src/types.rs` の `pub enum Direction {...}` の直後、`pub enum TradeMode {...}` の直前に以下を挿入：

```rust
/// 注文種別。戦略が Signal 生成時に選択する。
///
/// - `Market`: 成行注文。取引所がその瞬間の気配値で約定させる。
///   スリッページが発生しうるが、約定確実性が高い。
/// - `Limit { price }`: 指値注文。指定価格以下 (Long) / 以上 (Short)
///   でのみ約定する。未約定リスクあり。
///
/// JSON 形式は internally-tagged (`{"type": "market"}` /
/// `{"type": "limit", "price": "100.5"}`) — これは strategy ログや
/// /api/signals への出力でも人間可読性を保つため。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrderType {
    Market,
    Limit { price: Decimal },
}

impl Default for OrderType {
    fn default() -> Self {
        OrderType::Market
    }
}
```

- [ ] **Step 4: テストを実行してパスを確認**

```bash
cargo test -p auto-trader-core order_type 2>&1 | tail -20
```

Expected: `test result: ok. 4 passed`

- [ ] **Step 5: 全コアテストが通ることを確認**

```bash
cargo test -p auto-trader-core 2>&1 | tail -10
```

Expected: 全パス

- [ ] **Step 6: コミット**

```bash
git add crates/core/src/types.rs
git commit -m "$(cat <<'EOF'
feat(core): add OrderType enum (Market / Limit)

Introduce OrderType so strategies can express intent (成行/指値)
instead of the executor guessing. Internally-tagged JSON for
human-readable signal logs. Default = Market keeps existing
strategies unchanged once the field is wired into Signal.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `Signal` に `order_type` フィールドを追加する

**Files:**
- Modify: `crates/core/src/types.rs`

- [ ] **Step 1: 後方互換デシリアライズの失敗テストを書く**

`crates/core/src/types.rs` の `mod tests` ブロックに以下を追加：

```rust
    #[test]
    fn signal_defaults_order_type_to_market_when_absent() {
        // 既存コードが生成した Signal JSON (order_type フィールドなし)
        // は OrderType::Market に既定化されることを検証する。
        let legacy_json = r#"{
            "strategy_name": "legacy",
            "pair": "USD_JPY",
            "direction": "long",
            "entry_price": "150.00",
            "stop_loss": "149.50",
            "take_profit": "151.00",
            "confidence": 0.8,
            "timestamp": "2024-01-01T00:00:00Z",
            "allocation_pct": "0.5"
        }"#;
        let signal: Signal = serde_json::from_str(legacy_json).unwrap();
        assert!(matches!(signal.order_type, OrderType::Market));
    }

    #[test]
    fn signal_serializes_with_explicit_order_type() {
        let signal = Signal {
            strategy_name: "s".to_string(),
            pair: Pair::new("USD_JPY"),
            direction: Direction::Long,
            entry_price: dec!(150.0),
            stop_loss: dec!(149.0),
            take_profit: dec!(151.0),
            confidence: 0.8,
            timestamp: Utc::now(),
            allocation_pct: dec!(0.5),
            max_hold_until: None,
            order_type: OrderType::Limit { price: dec!(150.5) },
        };
        let json = serde_json::to_string(&signal).unwrap();
        assert!(json.contains(r#""order_type":{"type":"limit","price":"150.5"}"#));
    }
```

- [ ] **Step 2: テストを実行して失敗を確認**

```bash
cargo test -p auto-trader-core signal_ 2>&1 | tail -20
```

Expected: コンパイルエラー (`Signal` に `order_type` フィールドが存在しない)

- [ ] **Step 3: `Signal` に `order_type` フィールドを追加**

`crates/core/src/types.rs` の `pub struct Signal` を以下のように変更：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
    /// Fraction of leveraged account capacity the strategy wants to
    /// commit to this trade. Must be in (0, 1].
    ///
    /// The sizer turns this into a quantity via
    /// `floor((balance × leverage × allocation_pct / price) / min_lot)`.
    /// `allocation_pct` is the **only** sizing knob the strategy gets;
    /// chart-derived values (SL distance, ATR, …) intentionally do not
    /// influence quantity, matching the layering "signal = chart,
    /// execution = balance".
    #[serde(default = "default_allocation_pct")]
    pub allocation_pct: Decimal,
    /// Optional time-based fail-safe: position monitor will force-close
    /// the trade at this UTC time even if neither SL nor TP nor any
    /// strategy-driven exit has fired. Strategies use this to bound
    /// "stale" trades (e.g. mean-reversion 24h, vol-breakout 48h).
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
    /// 注文種別 (Market / Limit)。Signal を出した戦略が選ぶ。
    /// 既存の JSON を読み込むと Market に default される (後方互換)。
    #[serde(default)]
    pub order_type: OrderType,
}
```

- [ ] **Step 4: 既存テストのコンパイルエラーを修正**

`signal_roundtrip` テスト内の `Signal { ... }` リテラルに `order_type: OrderType::Market,` を追加する (同ファイル内で `use super::*;` 済みなので import 不要)。

```rust
    #[test]
    fn signal_roundtrip() {
        let signal = Signal {
            strategy_name: "test".to_string(),
            pair: Pair::new("USD_JPY"),
            direction: Direction::Long,
            entry_price: dec!(150.00),
            stop_loss: dec!(149.50),
            take_profit: dec!(151.00),
            confidence: 0.8,
            timestamp: Utc::now(),
            allocation_pct: dec!(0.5),
            max_hold_until: None,
            order_type: OrderType::Market,
        };
        let json = serde_json::to_string(&signal).unwrap();
        let back: Signal = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pair, signal.pair);
        assert_eq!(back.direction, Direction::Long);
    }
```

- [ ] **Step 5: テストを実行してパスを確認**

```bash
cargo test -p auto-trader-core 2>&1 | tail -10
```

Expected: 全パス（既存の `signal_deserialize_without_allocation_pct_falls_back_to_default` も引き続き通る）

- [ ] **Step 6: ワークスペース全体のコンパイルチェック（既存呼び出し側のエラーを洗い出す）**

```bash
cargo check --workspace 2>&1 | tail -50
```

Expected: `Signal { ... }` リテラルを持つ既存コード (strategy/swing_llm.rs など) でフィールド欠落エラー。エラー箇所を記録して Task 3 で順次修正。

- [ ] **Step 7: コミット (cargo check はここで通らなくてよい — Task 3 で修正する)**

```bash
git add crates/core/src/types.rs
git commit -m "$(cat <<'EOF'
feat(core): add order_type field to Signal

Default = OrderType::Market (serde(default)) so existing serialized
signals keep deserializing without migration. Strategies populate
it in the next commit.

Note: workspace cargo check fails after this commit because
existing Signal struct literals in strategies lack the new field.
Fixed in the immediately following commit (Task 3).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: 既存戦略を `OrderType::Market` に追従させる

**Files:**
- Modify: `crates/strategy/src/bb_mean_revert.rs`
- Modify: `crates/strategy/src/donchian_trend.rs`
- Modify: `crates/strategy/src/donchian_trend_evolve.rs`
- Modify: `crates/strategy/src/squeeze_momentum.rs`
- Modify: `crates/strategy/src/swing_llm.rs`

各戦略が `Signal { ... }` を構築している箇所すべてに `order_type: OrderType::Market,` を追加する。

- [ ] **Step 1: 対象箇所を洗い出す**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
grep -rn 'Signal {' crates/strategy/src 2>&1
```

Expected: `bb_mean_revert.rs`, `donchian_trend.rs`, `donchian_trend_evolve.rs`, `squeeze_momentum.rs`, `swing_llm.rs` の Signal 生成箇所が列挙される。

- [ ] **Step 2: bb_mean_revert の Long signal 生成箇所を修正**

`crates/strategy/src/bb_mean_revert.rs` のファイル先頭 use 節に (すでに Signal をインポートしているはず)：

```rust
use auto_trader_core::types::OrderType;
```

を `use auto_trader_core::types::{Signal, Direction, Pair}` のような既存行に `OrderType` を追加する形で挿入。既に `use auto_trader_core::types::*;` のようにワイルドカードがあれば不要。

続いて Long signal の `Signal { ... }` 構築箇所（`direction: Direction::Long` を含むブロック、プロジェクト現状では ~L130-147 付近）の `max_hold_until: Some(...)` の次行に:

```rust
                order_type: OrderType::Market,
```

を挿入。同ファイル内の Short 用 `Signal { ... }` 構築箇所（~L152-166 付近）にも同じく挿入。

- [ ] **Step 3: donchian_trend の Long/Short signal 生成箇所を修正**

`crates/strategy/src/donchian_trend.rs` 内の `Signal { ... }` 構築箇所 2 箇所（~L175-190 と ~L192-205）に `order_type: OrderType::Market,` を追加。use 節に `OrderType` を追加。

- [ ] **Step 4: donchian_trend_evolve の Long/Short signal 生成箇所を修正**

`crates/strategy/src/donchian_trend_evolve.rs` 内の `Signal { ... }` 構築箇所 2 箇所（~L158-172 と ~L174-186）に `order_type: OrderType::Market,` を追加。use 節に `OrderType` を追加。

- [ ] **Step 5: squeeze_momentum の Long/Short signal 生成箇所を修正**

`crates/strategy/src/squeeze_momentum.rs` 内の `Signal { ... }` 構築箇所 2 箇所（~L175-188 と ~L190-202）に `order_type: OrderType::Market,` を追加。use 節に `OrderType` を追加。

- [ ] **Step 6: swing_llm の signal 生成箇所を修正**

```bash
grep -n 'Signal {' crates/strategy/src/swing_llm.rs
```

出力に従って該当する `Signal { ... }` ブロック全てに `order_type: OrderType::Market,` を追加。use 節に `OrderType` を追加。

- [ ] **Step 7: ワークスペース全体をビルド**

```bash
cargo build --workspace 2>&1 | tail -20
```

Expected: コンパイル成功。残エラーがあればそれは Task 2 で検出されたがまだ対応していない呼び出し側。`grep -rn 'Signal {' crates` で全件洗い出して修正する。

- [ ] **Step 8: 全戦略テストが通ることを確認**

```bash
cargo test -p auto-trader-strategy 2>&1 | tail -20
```

Expected: 全パス（既存テスト内の Signal リテラルも同様に `order_type` を埋める必要がある。失敗したら該当テストに追加）

- [ ] **Step 9: ワークスペース全体のテストが通ることを確認**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: 全パス

- [ ] **Step 10: コミット**

```bash
git add crates/strategy/src
git commit -m "$(cat <<'EOF'
feat(strategy): set order_type = Market on all existing signals

All 4 crypto strategies (bb_mean_revert, donchian_trend,
donchian_trend_evolve, squeeze_momentum) and the FX swing_llm
strategy emit market orders. This matches the current paper
behavior where entry_price from signal is used as the fill price.

Once live executor lands, these strategies will naturally send
成行 to bitFlyer. Limit-order strategies can switch by returning
OrderType::Limit { price } from their signal builders.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `TradeStatus` に `Pending` / `Inconsistent` を追加する

**Files:**
- Modify: `crates/core/src/types.rs`
- Modify: `crates/db/src/trades.rs`

- [ ] **Step 1: 失敗するテストを書く**

`crates/core/src/types.rs` の `mod tests` に以下を追加：

```rust
    #[test]
    fn trade_status_serializes_pending() {
        let json = serde_json::to_string(&TradeStatus::Pending).unwrap();
        assert_eq!(json, r#""pending""#);
    }

    #[test]
    fn trade_status_serializes_inconsistent() {
        let json = serde_json::to_string(&TradeStatus::Inconsistent).unwrap();
        assert_eq!(json, r#""inconsistent""#);
    }

    #[test]
    fn trade_status_roundtrip_all_variants() {
        for variant in [
            TradeStatus::Open,
            TradeStatus::Closed,
            TradeStatus::Pending,
            TradeStatus::Inconsistent,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: TradeStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }
```

- [ ] **Step 2: テストを実行して失敗を確認**

```bash
cargo test -p auto-trader-core trade_status 2>&1 | tail -10
```

Expected: コンパイルエラー (バリアント欠落)

- [ ] **Step 3: `TradeStatus` を拡張**

`crates/core/src/types.rs` の以下の enum 定義を置き換える：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeStatus {
    /// 注文を取引所に送信済みで約定確認待ち (live のみ)
    Pending,
    /// 約定確認済み、保有中
    Open,
    /// 決済済み
    Closed,
    /// DB と取引所で状態が食い違い、手動対処が必要
    Inconsistent,
}
```

- [ ] **Step 4: テストを実行してパスを確認**

```bash
cargo test -p auto-trader-core trade_status 2>&1 | tail -10
```

Expected: `test result: ok. 3 passed`

- [ ] **Step 5: db crate の TradeStatus デシリアライズを拡張**

```bash
grep -n 'TradeStatus::\|"open"\|"closed"\|trade_status' crates/db/src/trades.rs | head -40
```

`crates/db/src/trades.rs` 内で DB の `status` 文字列から `TradeStatus` に変換している match 節を探す。典型的には以下のパターン：

```rust
let status = match status_str.as_str() {
    "open" => TradeStatus::Open,
    "closed" => TradeStatus::Closed,
    _ => anyhow::bail!("unknown trade status: {}", status_str),
};
```

これを以下に置き換える：

```rust
let status = match status_str.as_str() {
    "pending" => TradeStatus::Pending,
    "open" => TradeStatus::Open,
    "closed" => TradeStatus::Closed,
    "inconsistent" => TradeStatus::Inconsistent,
    _ => anyhow::bail!("unknown trade status: {}", status_str),
};
```

もし `serde` で自動 deserialize する形 (`#[sqlx(try_from = "String")]` など) ならコード変更は不要で、Step 6 へ。

- [ ] **Step 6: db crate のテストを実行**

```bash
cargo test -p auto-trader-db 2>&1 | tail -20
```

Expected: 全パス

- [ ] **Step 7: ワークスペース全体のコンパイルチェック**

```bash
cargo build --workspace 2>&1 | tail -10
```

Expected: 成功

- [ ] **Step 8: コミット**

```bash
git add crates/core/src/types.rs crates/db/src/trades.rs
git commit -m "$(cat <<'EOF'
feat(core,db): add TradeStatus::Pending / Inconsistent

Pending is used by live executor between 注文送信 and 約定確認.
Inconsistent flags trades where DB and exchange state diverged
and human intervention is required (reconciliation task will
mark positions here when the DB has an open trade but getpositions
does not).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `Trade` に bitFlyer 注文 ID を保持するフィールドを追加する

**Files:**
- Modify: `crates/core/src/types.rs`
- Modify: `crates/db/src/trades.rs`

- [ ] **Step 1: `Trade` 構造体にフィールド追加**

`crates/core/src/types.rs` の `pub struct Trade` を以下のように変更（新フィールドを2つ末尾に追加）:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: Uuid,
    pub strategy_name: String,
    pub pair: Pair,
    pub exchange: Exchange,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub quantity: Option<Decimal>,
    pub leverage: Decimal,
    pub fees: Decimal,
    pub paper_account_id: Option<Uuid>,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_pips: Option<Decimal>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<ExitReason>,
    pub mode: TradeMode,
    pub status: TradeStatus,
    /// Optional time-based fail-safe — see `Signal::max_hold_until`.
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
    /// bitFlyer 注文受付 ID (sendchildorder のレスポンス)。
    /// Paper トレードでは None。
    #[serde(default)]
    pub child_order_acceptance_id: Option<String>,
    /// bitFlyer 注文 ID (約定確定後に getchildorders から取得)。
    /// Paper トレードでは None。pending 中も None。
    #[serde(default)]
    pub child_order_id: Option<String>,
}
```

- [ ] **Step 2: 既存テストのコンパイルエラーを修正**

`cargo check --workspace` を走らせると `Trade { ... }` リテラルを持つ箇所がエラーになる。これを一つずつ修正：

```bash
cargo check --workspace 2>&1 | grep -E "error\[E0063\]|missing field" | head -20
```

エラー箇所全てに:

```rust
            child_order_acceptance_id: None,
            child_order_id: None,
```

を追加。代表的な場所:
- `crates/strategy/src/bb_mean_revert.rs` テスト内 (~L265-286)
- `crates/strategy/src/donchian_trend.rs` テスト内 (~L305-325)
- `crates/strategy/src/donchian_trend_evolve.rs` テスト内
- `crates/strategy/src/squeeze_momentum.rs` テスト内
- `crates/executor/src/paper.rs` の Trade 構築箇所
- `crates/db/src/trades.rs` insert / select の構築箇所
- `crates/app/src/api/positions.rs` (今回変更不要だが Trade を Read するので `..Default::default()` は使えない)

- [ ] **Step 3: db crate の Trade insert / select SQL を更新**

`crates/db/src/trades.rs` 内で `INSERT INTO trades` または `SELECT ... FROM trades` を実行している箇所を探す：

```bash
grep -n 'INSERT INTO trades\|SELECT.*FROM trades\|trade_row' crates/db/src/trades.rs | head -30
```

INSERT 文の列リストと VALUES に `child_order_acceptance_id`, `child_order_id` を追加：

```sql
INSERT INTO trades (
    id, strategy_name, pair, exchange, direction,
    entry_price, stop_loss, take_profit, quantity, leverage,
    fees, paper_account_id, entry_at, mode, status,
    max_hold_until, child_order_acceptance_id, child_order_id
) VALUES ($1, $2, $3, ..., $18)
```

SELECT 文は Task 6 のマイグレーション適用後に `*` 展開で自動取得されるが、明示列挙している場合は追加する。`sqlx::query!` マクロを使っている場合は compile-time チェックのためマイグレーション済みの DB に対して `cargo sqlx prepare` の再生成が必要 (後続 Step)。

構造体から DB 行にマッピングしているコード (`Trade { ... }` をクエリ結果から組み立てる箇所) に:

```rust
    child_order_acceptance_id: row.try_get("child_order_acceptance_id").ok(),
    child_order_id: row.try_get("child_order_id").ok(),
```

を追加。既存ロジックでカラム追加時の NULL 吸収をこの形でやっている前例を `grep -n 'try_get' crates/db/src/trades.rs` で確認して同じパターンで書く。

- [ ] **Step 4: db マイグレーション未適用でも `cargo build --workspace` が通るか確認**

```bash
cargo build --workspace 2>&1 | tail -10
```

Expected: 成功 (この段階ではスキーマ列が無いため実行時の INSERT は失敗するが、コンパイル時チェックは `sqlx::query_as` の静的検証が入るので DB が起動していれば `SQLX_OFFLINE=true` で通る)。

もし sqlx offline マクロで失敗したら、マイグレーション適用後（Task 6 の Step 5 以降）にリトライする形で「先にスキーマを入れる」順序に入れ替える。

- [ ] **Step 5: コミット (DB マイグレーションはまだ — Task 6 で追加)**

```bash
git add crates/core/src/types.rs crates/db/src/trades.rs crates/strategy/src crates/executor/src
git commit -m "$(cat <<'EOF'
feat(core,db): Trade gains child_order_acceptance_id / child_order_id

Nullable bitFlyer order identifiers. Populated only by live executor.
Paper trades always carry None. Schema migration adds the matching
columns in the next commit.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: DB マイグレーションを追加する

**Files:**
- Create: `migrations/20260414000001_live_trading_support.sql`

- [ ] **Step 1: 既存の trade_status スキーマを調査**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
docker compose exec -T db psql -U auto-trader -d auto_trader -c "\d+ trades" 2>&1 | head -40
```

`status` 列が `text`（CHECK 制約つき）か `trade_status` enum 型かを確認。出力を plan のメモとして控える。

```bash
docker compose exec -T db psql -U auto-trader -d auto_trader -c "SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'trades'::regclass;" 2>&1
```

CHECK 制約の内容も確認。

- [ ] **Step 2: マイグレーションファイルを作成**

`migrations/20260414000001_live_trading_support.sql` を新規作成。**DB の status 列の型に応じて片方を採用する**:

**パターン A: status が `trade_status` PostgreSQL enum 型の場合:**

```sql
-- Live trading support schema additions.
-- - Extend trade_status with 'pending' (order sent, waiting on fill)
--   and 'inconsistent' (DB ↔ exchange divergence, manual fix needed).
-- - Add bitFlyer child order identifiers on trades.
-- - Partial unique index: at most one active (pending+open) trade per
--   (account, strategy, pair) — prevents duplicate entries after restart.
-- - risk_halts table: persists Kill Switch activations so restarts
--   re-apply existing halts.

ALTER TYPE trade_status ADD VALUE IF NOT EXISTS 'pending';
ALTER TYPE trade_status ADD VALUE IF NOT EXISTS 'inconsistent';

ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS child_order_acceptance_id TEXT,
    ADD COLUMN IF NOT EXISTS child_order_id TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS trades_one_active_per_strategy_pair
    ON trades (paper_account_id, strategy_name, pair)
    WHERE status IN ('pending', 'open');

CREATE TABLE IF NOT EXISTS risk_halts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    paper_account_id UUID NOT NULL REFERENCES paper_accounts(id),
    reason TEXT NOT NULL,
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    halted_until TIMESTAMPTZ NOT NULL,
    released_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS risk_halts_account_active
    ON risk_halts (paper_account_id, halted_until)
    WHERE released_at IS NULL;
```

**パターン B: status が `TEXT` + CHECK 制約の場合:**

```sql
-- 既存 CHECK 制約を差し替える
ALTER TABLE trades DROP CONSTRAINT IF EXISTS trades_status_check;
ALTER TABLE trades
    ADD CONSTRAINT trades_status_check
    CHECK (status IN ('pending', 'open', 'closed', 'inconsistent'));

ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS child_order_acceptance_id TEXT,
    ADD COLUMN IF NOT EXISTS child_order_id TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS trades_one_active_per_strategy_pair
    ON trades (paper_account_id, strategy_name, pair)
    WHERE status IN ('pending', 'open');

CREATE TABLE IF NOT EXISTS risk_halts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    paper_account_id UUID NOT NULL REFERENCES paper_accounts(id),
    reason TEXT NOT NULL,
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    halted_until TIMESTAMPTZ NOT NULL,
    released_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS risk_halts_account_active
    ON risk_halts (paper_account_id, halted_until)
    WHERE released_at IS NULL;
```

※ 上の制約名 `trades_status_check` は Step 1 で確認した実際の名前に置き換える (存在しない場合は `IF EXISTS` で no-op になる)。

- [ ] **Step 3: 既存の open/pending 重複が無いことを事前検査**

partial unique index は既存データに違反があれば CREATE INDEX に失敗する。適用前に検査:

```bash
docker compose exec -T db psql -U auto-trader -d auto_trader -c "
SELECT paper_account_id, strategy_name, pair, COUNT(*) 
FROM trades 
WHERE status IN ('pending', 'open') 
GROUP BY 1,2,3 
HAVING COUNT(*) > 1;
" 2>&1
```

Expected: 0 rows。もし重複が検出されたら plan を一時停止してユーザーに相談（手動で 1 件を closed にする等）。

- [ ] **Step 4: マイグレーションを適用**

```bash
docker compose up -d db 2>&1 | tail -5
docker compose exec -T db psql -U auto-trader -d auto_trader -f /dev/stdin < migrations/20260414000001_live_trading_support.sql 2>&1
```

※ プロジェクトが sqlx migrate を使っているなら:

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
DATABASE_URL=postgresql://auto-trader:auto-trader@localhost:15432/auto_trader \
  sqlx migrate run --source migrations 2>&1 | tail -10
```

Expected: `Applied 20260414000001/migrate live trading support`

- [ ] **Step 5: スキーマが反映されたことを確認**

```bash
docker compose exec -T db psql -U auto-trader -d auto_trader -c "\d+ trades" 2>&1 | grep -E 'child_order|status'
docker compose exec -T db psql -U auto-trader -d auto_trader -c "\d risk_halts" 2>&1
docker compose exec -T db psql -U auto-trader -d auto_trader -c "SELECT indexname FROM pg_indexes WHERE tablename = 'trades';" 2>&1 | grep trades_one_active
```

Expected:
- `child_order_acceptance_id`, `child_order_id` 列が存在
- `risk_halts` テーブルが存在
- `trades_one_active_per_strategy_pair` インデックスが存在

- [ ] **Step 6: sqlx のコンパイル時チェック用メタデータを再生成 (プロジェクトが使っていれば)**

```bash
ls .sqlx 2>&1
```

`.sqlx` ディレクトリがあれば:

```bash
DATABASE_URL=postgresql://auto-trader:auto-trader@localhost:15432/auto_trader \
  cargo sqlx prepare --workspace 2>&1 | tail -10
```

Expected: `query data written to .sqlx`

無ければスキップ。

- [ ] **Step 7: マイグレーション適用後、`#[sqlx(default)]` を外し SELECT に新列を追加する (Batch B レビュー I-2 対応)**

Batch B で `crates/db/src/trades.rs` の `TradeRow` に `#[sqlx(default)]` を付けた理由は、マイグレーション未適用の DB でも SELECT が落ちないようにするため。本 Task で列が追加された今、`#[sqlx(default)]` は**外す**。理由は「将来うっかり `DROP COLUMN` すると silently `None` に fallback し、約定済み注文 ID が失われる事故」を防ぐため。

1. `grep -n '#\[sqlx(default)\]' crates/db/src/trades.rs` で付与箇所を確認
2. `child_order_acceptance_id` と `child_order_id` に付いている `#[sqlx(default)]` のみを削除 (他フィールドはそのまま)
3. SELECT 文のうち列を明示列挙している箇所 (`SELECT id, strategy_name, ... FROM trades`) 全てに `child_order_acceptance_id` / `child_order_id` を追加
4. `grep -n 'SELECT' crates/db/src/trades.rs` で対象クエリ列挙

- [ ] **Step 8: ワークスペースビルドとテスト**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -15
```

Expected: 全パス。`#[sqlx(default)]` を外したことで DB 接続テストが必要になる場合は `DATABASE_URL=postgresql://auto-trader:auto-trader@localhost:15432/auto_trader` を付けて実行。

- [ ] **Step 9: Paper trade の INSERT が実 DB で通ることを verify**

マイグレーション適用後、paper trade の insert→select が一気通貫で壊れていないことを手動検証:

```bash
# テスト用の temporary trade を insert して select できることを確認
docker compose exec -T db psql -U auto-trader -d auto_trader <<'SQL'
BEGIN;
INSERT INTO trades (
    id, strategy_name, pair, exchange, direction,
    entry_price, stop_loss, take_profit, quantity, leverage,
    fees, paper_account_id, entry_at, mode, status,
    max_hold_until, child_order_acceptance_id, child_order_id
) VALUES (
    gen_random_uuid(), 'test_smoke', 'FX_BTC_JPY', 'bitflyer_cfd', 'long',
    11000000, 10700000, 11300000, 0.001, 2,
    0, 'a0000000-0000-0000-0000-000000000011', NOW(), 'paper', 'open',
    NULL, NULL, NULL
);
SELECT id, status, child_order_acceptance_id, child_order_id FROM trades WHERE strategy_name = 'test_smoke';
ROLLBACK;
SQL
```

Expected: INSERT 成功、SELECT で status=open / 注文 ID カラムが NULL で見える。

- [ ] **Step 10: コミット (マイグレーション + sqlx default 撤去 + SELECT 列追加)**

```bash
git add migrations/20260414000001_live_trading_support.sql crates/db/src/trades.rs .sqlx 2>/dev/null
git commit -m "$(cat <<'EOF'
feat(db): schema for live trading (pending/inconsistent, order ids, risk_halts)

- trade_status (enum or CHECK) gains 'pending' and 'inconsistent'
- trades.child_order_acceptance_id / child_order_id (nullable text)
- partial unique index prevents duplicate active positions per
  (paper_account_id, strategy_name, pair)
- risk_halts persists Kill Switch activations across restarts

Also removes the transitional #[sqlx(default)] on child_order_*
columns now that the migration is in place: keeping the default
would hide DROP COLUMN regressions and silently lose live order
IDs. SELECT queries enumerate the new columns explicitly.

NOTE: this migration and the preceding Trade-struct commit MUST
ship in the same PR — they form an inseparable pair.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `notify` crate を新設する

**Files:**
- Create: `crates/notify/Cargo.toml`
- Create: `crates/notify/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: crate ディレクトリを作成**

```bash
mkdir -p /Users/ryugo/Developer/src/personal/auto-trader/crates/notify/src
mkdir -p /Users/ryugo/Developer/src/personal/auto-trader/crates/notify/tests
```

- [ ] **Step 2: `crates/notify/Cargo.toml` を作成**

```toml
[package]
name = "auto-trader-notify"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
tokio = { workspace = true }
reqwest = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
rust_decimal = { workspace = true }
uuid = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
async-trait = { workspace = true }

auto-trader-core = { path = "../core" }

[dev-dependencies]
wiremock = "0.6"
```

- [ ] **Step 3: workspace Cargo.toml に crate を追加**

`/Users/ryugo/Developer/src/personal/auto-trader/Cargo.toml` の `[workspace] members` セクションに `"crates/notify",` を追加：

```toml
[workspace]
resolver = "2"
members = [
    "crates/core",
    "crates/db",
    "crates/macro-analyst",
    "crates/market",
    "crates/strategy",
    "crates/executor",
    "crates/app",
    "crates/vegapunk-client",
    "crates/backtest",
    "crates/notify",
]
```

同じファイルの `[workspace.dependencies]` に `wiremock = "0.6"` を追加（後続 PR でも使うので予約）:

```toml
wiremock = "0.6"
```

- [ ] **Step 4: `crates/notify/src/lib.rs` の骨格を書く**

```rust
//! 外部通知チャネル (Slack Webhook など)。
//!
//! `db::notifications` はアプリ内通知（UI ベル表示）専用で、オペレータが
//! 外部で気付く通知はこの crate が担う。本 PR では Slack Webhook の
//! 送信のみを実装し、発火ポイント (`LiveTrader` / `RiskGate` / reconciler
//! など) は後続 PR で配線する。

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct OrderFilledEvent {
    pub account_name: String,
    pub trade_id: Uuid,
    pub pair: String,
    pub direction: String,
    pub quantity: Decimal,
    pub price: Decimal,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderFailedEvent {
    pub account_name: String,
    pub strategy_name: String,
    pub pair: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionClosedEvent {
    pub account_name: String,
    pub trade_id: Uuid,
    pub pnl_amount: Decimal,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KillSwitchTriggeredEvent {
    pub account_name: String,
    pub daily_loss: Decimal,
    pub limit: Decimal,
    pub halted_until: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KillSwitchReleasedEvent {
    pub account_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebSocketDisconnectedEvent {
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartupReconciliationDiffEvent {
    pub orphan_db_trade_ids: Vec<Uuid>,
    pub orphan_exchange_positions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BalanceDriftEvent {
    pub account_name: String,
    pub db_balance: Decimal,
    pub exchange_balance: Decimal,
    pub diff_pct: Decimal,
}

#[derive(Debug, Clone, Serialize)]
pub struct DryRunOrderEvent {
    pub account_name: String,
    pub strategy_name: String,
    pub pair: String,
    pub direction: String,
    pub quantity: Decimal,
    pub intended_price: Decimal,
}

/// 通知イベント。Slack には各イベントごとに整形された文面で送る。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotifyEvent {
    OrderFilled(OrderFilledEvent),
    OrderFailed(OrderFailedEvent),
    PositionClosed(PositionClosedEvent),
    KillSwitchTriggered(KillSwitchTriggeredEvent),
    KillSwitchReleased(KillSwitchReleasedEvent),
    WebSocketDisconnected(WebSocketDisconnectedEvent),
    StartupReconciliationDiff(StartupReconciliationDiffEvent),
    BalanceDrift(BalanceDriftEvent),
    DryRunOrder(DryRunOrderEvent),
}

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("slack returned non-2xx status: {0}")]
    Status(u16),
}

/// Slack Webhook 送信クライアント。`slack_webhook_url` が None なら
/// no-op（ログのみ）。通知失敗は本業務を止めないため、送信失敗は
/// warn ログに留め、呼び出し側に Result を返しつつも実運用では
/// 結果を無視してよい設計。
#[derive(Clone)]
pub struct Notifier {
    slack_webhook_url: Option<String>,
    http: reqwest::Client,
}

impl Notifier {
    pub fn new(slack_webhook_url: Option<String>) -> Self {
        Self {
            slack_webhook_url,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client builder should not fail with basic config"),
        }
    }

    /// URL を差し替えたい（テスト用）場合のコンストラクタ。
    pub fn with_client(slack_webhook_url: Option<String>, http: reqwest::Client) -> Self {
        Self {
            slack_webhook_url,
            http,
        }
    }

    pub async fn send(&self, event: NotifyEvent) -> Result<(), NotifyError> {
        let Some(url) = &self.slack_webhook_url else {
            tracing::debug!(?event, "notify: slack webhook not configured, skipping");
            return Ok(());
        };
        let text = format_for_slack(&event);
        let body = serde_json::json!({ "text": text });
        let resp = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(NotifyError::Http)?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(status = status.as_u16(), "notify: slack returned non-2xx");
            return Err(NotifyError::Status(status.as_u16()));
        }
        Ok(())
    }
}

fn format_for_slack(event: &NotifyEvent) -> String {
    match event {
        NotifyEvent::OrderFilled(e) => format!(
            "✅ *約定* `{}` {} {} {} @ {} (trade {})",
            e.account_name, e.pair, e.direction, e.quantity, e.price, e.trade_id
        ),
        NotifyEvent::OrderFailed(e) => format!(
            "❌ *発注失敗* `{}` {} {} — {}",
            e.account_name, e.strategy_name, e.pair, e.reason
        ),
        NotifyEvent::PositionClosed(e) => format!(
            "🔒 *クローズ* `{}` pnl={} reason={} (trade {})",
            e.account_name, e.pnl_amount, e.reason, e.trade_id
        ),
        NotifyEvent::KillSwitchTriggered(e) => format!(
            "🛑 *Kill Switch 発動* `{}` 日次損失 {} / 上限 {} — 再開予定 {}",
            e.account_name, e.daily_loss, e.limit, e.halted_until
        ),
        NotifyEvent::KillSwitchReleased(e) => format!("🟢 *Kill Switch 解除* `{}`", e.account_name),
        NotifyEvent::WebSocketDisconnected(e) => {
            format!("⚠️ *WebSocket 切断* {} 秒", e.duration_secs)
        }
        NotifyEvent::StartupReconciliationDiff(e) => format!(
            "⚠️ *起動時リコン差分* DB のみ={} 件, 取引所のみ={} 件",
            e.orphan_db_trade_ids.len(),
            e.orphan_exchange_positions.len()
        ),
        NotifyEvent::BalanceDrift(e) => format!(
            "⚠️ *残高ズレ* `{}` DB={} / 取引所={} ({}%)",
            e.account_name, e.db_balance, e.exchange_balance, e.diff_pct
        ),
        NotifyEvent::DryRunOrder(e) => format!(
            "🧪 *DRY RUN* `{}` {} {} {} {} @ {} (発注せず)",
            e.account_name, e.strategy_name, e.pair, e.direction, e.quantity, e.intended_price
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn format_order_filled() {
        let ev = NotifyEvent::OrderFilled(OrderFilledEvent {
            account_name: "通常".into(),
            trade_id: Uuid::nil(),
            pair: "FX_BTC_JPY".into(),
            direction: "long".into(),
            quantity: dec!(0.01),
            price: dec!(11500000),
            at: Utc::now(),
        });
        let s = format_for_slack(&ev);
        assert!(s.contains("約定"));
        assert!(s.contains("通常"));
        assert!(s.contains("FX_BTC_JPY"));
        assert!(s.contains("11500000"));
    }

    #[test]
    fn format_kill_switch_triggered() {
        let ev = NotifyEvent::KillSwitchTriggered(KillSwitchTriggeredEvent {
            account_name: "通常".into(),
            daily_loss: dec!(-1500),
            limit: dec!(-1500),
            halted_until: Utc::now(),
        });
        let s = format_for_slack(&ev);
        assert!(s.contains("Kill Switch"));
        assert!(s.contains("通常"));
    }

    #[tokio::test]
    async fn send_without_webhook_is_noop() {
        let n = Notifier::new(None);
        let ev = NotifyEvent::WebSocketDisconnected(WebSocketDisconnectedEvent {
            duration_secs: 30,
        });
        // webhook が None なので即 Ok(())
        n.send(ev).await.unwrap();
    }
}
```

- [ ] **Step 5: 単体テストが通ることを確認**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
cargo test -p auto-trader-notify 2>&1 | tail -10
```

Expected: `test result: ok. 3 passed`

- [ ] **Step 6: ワークスペース全体のビルドが通ることを確認**

```bash
cargo build --workspace 2>&1 | tail -5
```

Expected: 成功

- [ ] **Step 7: コミット**

```bash
git add Cargo.toml crates/notify
git commit -m "$(cat <<'EOF'
feat(notify): new crate for Slack webhook notifications

Scaffolds NotifyEvent enum covering live-trading lifecycle events
(order filled/failed, position closed, kill switch, ws drop,
reconciliation diff, balance drift, dry-run order). Notifier.send
is a no-op when SLACK_WEBHOOK_URL is unset so existing dev setups
keep working. Wiring into LiveTrader / RiskGate comes in later PRs.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Slack Webhook 送信の wiremock 統合テスト

**Files:**
- Create: `crates/notify/tests/slack_integration_test.rs`

- [ ] **Step 1: テストファイルを作成**

`crates/notify/tests/slack_integration_test.rs`:

```rust
//! Slack Webhook 送信の統合テスト。wiremock でダミーの Slack サーバーを
//! 立ち上げ、Notifier が適切なペイロードを POST することを検証する。

use auto_trader_notify::*;
use rust_decimal_macros::dec;
use uuid::Uuid;
use wiremock::matchers::{body_json_schema, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn notifier_posts_text_payload_to_slack_url() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let notifier = Notifier::new(Some(server.uri()));
    let ev = NotifyEvent::OrderFilled(OrderFilledEvent {
        account_name: "通常".into(),
        trade_id: Uuid::nil(),
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.005),
        price: dec!(11500000),
        at: chrono::Utc::now(),
    });

    notifier.send(ev).await.expect("send should succeed against 200");
    // Mock::expect(1) が満たされなければドロップ時に panic する
}

#[tokio::test]
async fn notifier_returns_error_on_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let notifier = Notifier::new(Some(server.uri()));
    let ev = NotifyEvent::WebSocketDisconnected(WebSocketDisconnectedEvent {
        duration_secs: 42,
    });
    let err = notifier.send(ev).await.unwrap_err();
    match err {
        NotifyError::Status(code) => assert_eq!(code, 500),
        other => panic!("expected Status(500), got {:?}", other),
    }
}

#[tokio::test]
async fn notifier_noop_when_url_none() {
    let notifier = Notifier::new(None);
    let ev = NotifyEvent::KillSwitchReleased(KillSwitchReleasedEvent {
        account_name: "通常".into(),
    });
    notifier.send(ev).await.expect("noop should return Ok");
}
```

- [ ] **Step 2: テスト実行**

```bash
cargo test -p auto-trader-notify --tests 2>&1 | tail -15
```

Expected: `test result: ok. 3 passed` (integration test 3 本)

- [ ] **Step 3: コミット**

```bash
git add crates/notify/tests
git commit -m "$(cat <<'EOF'
test(notify): wiremock integration tests for Slack posting

Covers happy path (200), server error (500 → NotifyError::Status),
and no-webhook-configured (noop). Uses wiremock from workspace
devDependency.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: 設定ファイル (`config/default.toml` / `.env.example`) を更新する

**Files:**
- Modify: `.env.example`
- Modify: `config/default.toml`
- Modify: `crates/core/src/config.rs`

- [ ] **Step 1: 既存の `.env.example` を確認**

```bash
cat /Users/ryugo/Developer/src/personal/auto-trader/.env.example
```

- [ ] **Step 2: `.env.example` に必要な envs を追記**

既存内容の末尾に以下を追加:

```
# --- Live trading (bitFlyer) ---
# bitFlyer Lightning API キー。開発中は空のままで可。
# Live 稼働時に Web → 発行 → 1Password / direnv 経由で設定する。
BITFLYER_API_KEY=
BITFLYER_API_SECRET=

# --- Notifications ---
# Slack Incoming Webhook URL。未設定なら通知は no-op（ログのみ）。
SLACK_WEBHOOK_URL=

# --- Live execution toggle ---
# true (デフォルト): LiveTrader が発注直前で no-op する dry-run モード。
# false: 実発注。config/default.toml の [live].enabled = true と両立必要。
LIVE_DRY_RUN=true
```

- [ ] **Step 3: `config/default.toml` に `[risk]` と `[live]` を追加**

既存の末尾 (全戦略定義の後) に追加:

```toml
# === Risk / Kill Switch ===
# 全アカウント共通のガードレール。paper/live を問わず RiskGate
# （PR 4 で実装）が前段でシグナルをブロックする。
[risk]
daily_loss_limit_pct = 0.05      # 初期残高比 -5% で当日停止
price_freshness_secs = 60        # price tick がこの秒数より古ければ新規発注禁止
kill_switch_release_jst_hour = 0 # 翌 0:00 JST に自動解除

# === Live execution ===
# enabled = true でない限り LiveTrader は起動しない。
# dry_run が true の間は発注手前で no-op 通知のみ。
# LIVE_DRY_RUN env が設定されていればそちらが優先される。
[live]
enabled = false
dry_run = true
execution_poll_interval_secs = 3
reconciler_interval_secs = 300
balance_sync_interval_secs = 300
```

- [ ] **Step 4: `BitflyerConfig` に秘匿設定用フィールドを追加**

`crates/core/src/config.rs` の `BitflyerConfig` を以下に差し替え:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct BitflyerConfig {
    pub ws_url: String,
    pub api_url: String,
    /// BITFLYER_API_KEY env から埋める。config/default.toml には書かない。
    #[serde(skip, default)]
    pub api_key: Option<String>,
    /// BITFLYER_API_SECRET env から埋める。config/default.toml には書かない。
    #[serde(skip, default)]
    pub api_secret: Option<String>,
}
```

- [ ] **Step 5: `RiskConfig` / `LiveConfig` 型と `AppConfig` への配線**

`crates/core/src/config.rs` の `AppConfig` に以下フィールドを追加:

```rust
    #[serde(default)]
    pub risk: Option<RiskConfig>,
    #[serde(default)]
    pub live: Option<LiveConfig>,
```

同ファイル末尾（`impl AppConfig` の直前）に以下を挿入:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    /// 本日 (JST) のクローズ済み pnl 合計がこの比率 (×初期残高) を
    /// 下回ったら Kill Switch 発動。
    pub daily_loss_limit_pct: Decimal,
    /// price tick の鮮度上限 (秒)。超過時は新規シグナル拒否。
    pub price_freshness_secs: u64,
    /// Kill Switch を自動解除する JST 時刻 (0 = 0:00)。
    pub kill_switch_release_jst_hour: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LiveConfig {
    /// true の時のみ LiveTrader を起動する。account_type='live' の
    /// アカウントが存在すれば main.rs 起動時に true でなければ fatal。
    pub enabled: bool,
    /// true 中は発注直前で no-op し通知のみ出す (DryRunTrader)。
    /// LIVE_DRY_RUN env が設定されていれば env 優先。
    pub dry_run: bool,
    pub execution_poll_interval_secs: u64,
    pub reconciler_interval_secs: u64,
    pub balance_sync_interval_secs: u64,
}
```

- [ ] **Step 6: `main.rs` での env 吸い上げロジックを追加**

既存の config 読み込み直後、`bitflyer` フィールドを触るコード例を真似て、env から値を注入する:

```bash
grep -n 'AppConfig::load\|config.bitflyer\|BITFLYER\|OANDA_API_KEY' crates/app/src/main.rs | head -20
```

該当箇所（`AppConfig::load(...)` の直後）に以下を挿入:

```rust
    if let Some(bf) = config.bitflyer.as_mut() {
        bf.api_key = std::env::var("BITFLYER_API_KEY").ok().filter(|s| !s.is_empty());
        bf.api_secret = std::env::var("BITFLYER_API_SECRET").ok().filter(|s| !s.is_empty());
    }
```

※ `config` が `&AppConfig` で immutable な場合、`let mut config = AppConfig::load(...)?;` に変更する必要あり。

- [ ] **Step 7: 既存 config ユニットテストを更新**

`crates/core/src/config.rs` の既存 `parse_config_with_crypto` テストに対して、新フィールドは `#[serde(default)]` なので既存 TOML は壊れないはず。`cargo test -p auto-trader-core config` で確認。

さらに新規テストを追加:

```rust
    #[test]
    fn parse_config_with_risk_and_live() {
        let toml_str = r#"
[vegapunk]
endpoint = "http://localhost:3000"
schema = "fx-trading"

[database]
url = "postgresql://u:p@localhost/auto_trader"

[monitor]
interval_secs = 60

[pairs]
active = ["USD_JPY"]

[risk]
daily_loss_limit_pct = 0.05
price_freshness_secs = 60
kill_switch_release_jst_hour = 0

[live]
enabled = false
dry_run = true
execution_poll_interval_secs = 3
reconciler_interval_secs = 300
balance_sync_interval_secs = 300
"#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        let risk = cfg.risk.expect("risk section should parse");
        assert_eq!(risk.price_freshness_secs, 60);
        let live = cfg.live.expect("live section should parse");
        assert!(!live.enabled);
        assert!(live.dry_run);
    }

    #[test]
    fn bitflyer_config_api_key_starts_as_none() {
        // config ファイル側で書いても #[serde(skip)] で無視される
        let toml_str = r#"
[vegapunk]
endpoint = "x"
schema = "x"
[database]
url = "x"
[monitor]
interval_secs = 1
[pairs]
active = []
[bitflyer]
ws_url = "wss://example"
api_url = "https://example"
api_key = "LEAKED"
api_secret = "LEAKED"
"#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        let bf = cfg.bitflyer.unwrap();
        assert_eq!(bf.ws_url, "wss://example");
        assert!(bf.api_key.is_none(), "api_key must only come from env");
        assert!(bf.api_secret.is_none(), "api_secret must only come from env");
    }
```

- [ ] **Step 8: テスト実行**

```bash
cargo test -p auto-trader-core config 2>&1 | tail -15
```

Expected: 全パス

- [ ] **Step 9: ワークスペースビルドとテスト**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -15
```

Expected: 全パス

- [ ] **Step 10: コミット**

```bash
git add .env.example config/default.toml crates/core/src/config.rs crates/app/src/main.rs
git commit -m "$(cat <<'EOF'
feat(config): [risk] and [live] sections + bitFlyer secret envs

- [risk]: daily_loss_limit_pct (5%), price_freshness_secs (60),
  kill_switch_release_jst_hour (0). Consumed by RiskGate in PR 4.
- [live]: enabled/dry_run + task intervals. LiveTrader only starts
  when enabled = true. LIVE_DRY_RUN env overrides dry_run.
- BitflyerConfig.api_key/api_secret are serde(skip) so they can
  only be populated from BITFLYER_API_KEY / BITFLYER_API_SECRET
  env vars — config files (incl. accidental commits) can never
  hold real secrets.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: 最終検証と CI グリーン確認

- [ ] **Step 1: 全 crate でテスト実行**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
cargo test --workspace 2>&1 | tail -30
```

Expected: 全パス

- [ ] **Step 2: clippy 実行**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30
```

Expected: warning ゼロ。発生した場合は警告内容を確認し修正する。

- [ ] **Step 3: fmt チェック**

```bash
cargo fmt --all -- --check 2>&1 | tail -10
```

Expected: 差分ゼロ。差分があれば `cargo fmt --all` を実行してコミット。

- [ ] **Step 4: `simplify` スキルで変更コードを見直す**

`simplify` スキルを起動し、追加した型・関数・構造体を見直す。YAGNI 違反（早すぎる抽象化、不要フィールド、未使用 enum variant）を削減。

- [ ] **Step 5: `code-review` スキルを実行**

CLAUDE.md 厳守ルールに従い `code-review` スキルで変更全体をレビュー。指摘事項があれば修正コミットを追加。

- [ ] **Step 6: Docker 上で auto-trader を起動して既存ペーパートレード挙動が壊れていないことを確認**

```bash
docker compose build --no-cache auto-trader 2>&1 | tail -10
docker compose up -d 2>&1 | tail -10
sleep 10
docker compose logs --tail=30 auto-trader 2>&1 | tail -30
```

Expected: 起動成功、`API server listening on 0.0.0.0:3001`、`bitflyer websocket connected` が出る。エラーログ無し。

- [ ] **Step 7: 管理画面が表示されることを確認**

```bash
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:3001/positions
```

Expected: `200`

- [ ] **Step 8: PR を作成**

CLAUDE.md 厳守ルール: `superpowers:finishing-a-development-branch` を起動。その手順に従って GitHub に push し PR を作成。PR 本文には以下を含める:

- 目的 (live trading の土台、発注挙動の変更なし)
- 変更ファイル一覧 (10 task 分)
- テスト結果
- リスク評価: **ペーパートレードの挙動は変更なし。新 enum / 列 / 設定は全て opt-in / 後方互換**
- 次 PR へのリンク予告 (`BitflyerPrivateApi` 実装)

---

## Self-Review Notes

- [x] `OrderType` / `TradeStatus::Pending`/`Inconsistent` / `Trade.child_order_*` / DB 列 / unique index / risk_halts / notify crate / [risk] / [live] / secret env — 全て Task 化済み
- [x] 既存 Signal/Trade リテラルの破壊的変更には Task 3/5 で一括対応
- [x] 後方互換: `#[serde(default)]` による JSON 互換、`IF NOT EXISTS` / `ADD VALUE IF NOT EXISTS` による DB 互換
- [x] ペーパー経路の挙動変更ゼロ（新 enum は opt-in、LiveTrader は本 PR に含めない）
- [x] DB 適用順序: マイグレーションは Task 6 で適用、sqlx offline があれば再生成
- [x] テストは TDD（失敗 → 実装 → パス）の順

## After This PR

次 PR (`PR 2: BitflyerPrivateApi`) では:
- `hmac` / `sha2` / `hex` / `governor` を workspace deps に正式追加
- `crates/market/src/bitflyer_private.rs` 新設
- HMAC 署名スナップショットテスト
- wiremock で bitFlyer API レスポンスを mock した統合テスト
