# PR-2: RiskGate + Reconciler + BalanceSync 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:subagent-driven-development` (推奨) または `superpowers:executing-plans` を使ってタスク単位で実装すること。ステップはチェックボックス (`- [ ]`) で進捗管理。

**Goal:** live 口座向けの前段ガード (RiskGate) / 起動時・定期リコンシリエーション / 残高同期を追加し、PR-1 の Unified Trader をそのまま本番発注に耐える状態まで持っていく。

**Architecture:** Unified Trader (PR-1) は既に `send_child_order` + `poll_executions` で同期的に約定を待つため、pending/inconsistent 状態の state machine と `ExecutionPollingTask` は不要。代わりに `poll_executions` タイムアウトの残骸は Reconciler が検出。RiskGate は Signal → Executor の間に差し込む純関数的ガード (DB/PriceStore 参照)。Reconciler/BalanceSync は live アカウント限定のバックグラウンドタスク。

**Tech Stack:** Rust (tokio / sqlx / rust_decimal), Postgres (partial unique index, FK), bitFlyer Private REST API (既存 `BitflyerPrivateApi`), Slack Webhook (既存 `Notifier`), wiremock (統合テスト).

**前提 (PR-1 マージ済み):**
- `UnifiedTrader` (`crates/executor/src/trader.rs`) が `dry_run` 分岐で paper/live を統一実装
- `TradeStatus = Open | Closing | Closed` (pending/inconsistent 無し)
- `BitflyerPrivateApi` に `send_child_order` / `get_executions` / `get_positions` / `get_collateral` / `cancel_child_order` 全て存在
- `LiveConfig { enabled, dry_run, execution_poll_interval_secs, reconciler_interval_secs, balance_sync_interval_secs }` 定義済み
- `NotifyEvent` に `KillSwitchTriggered` / `KillSwitchReleased` / `StartupReconciliationDiff` / `BalanceDrift` / `WebSocketDisconnected` 全て存在
- `LIVE_DRY_RUN` env + `[live].dry_run` 二段ゲート + 起動時 live-account gate (main.rs) 設定済み
- 翌日 overnight fee は `account_type == "paper"` のみ適用 (main.rs:1517)
- `risk_halts` テーブルは PR-1 で drop 済み → PR-2 で再作成

---

## File Structure

### 新規作成
- `migrations/20260416000001_risk_halts.sql` — `risk_halts` テーブル再作成
- `crates/db/src/risk_halts.rs` — `insert_halt` / `active_halt_for_account` / `release_halt` / `list_daily_pnl`
- `crates/executor/src/risk_gate.rs` — `RiskGate` / `GateDecision` / `RejectReason` + pure-logic 評価
- `crates/executor/tests/risk_gate_test.rs` — ユニット + 軽量統合
- `crates/app/src/tasks/mod.rs` — `pub mod reconciler; pub mod balance_sync;`
- `crates/app/src/tasks/reconciler.rs` — 起動時 + 定期実行の差分検出
- `crates/app/src/tasks/balance_sync.rs` — 定期残高同期
- `crates/app/tests/reconciler_test.rs` — wiremock 統合テスト
- `crates/app/tests/balance_sync_test.rs` — wiremock 統合テスト

### 修正
- `crates/db/src/lib.rs` — `pub mod risk_halts;` 追加
- `crates/executor/src/lib.rs` — `pub mod risk_gate;` 追加
- `crates/app/src/lib.rs` (or `main.rs` で `mod tasks;`) — tasks モジュール公開
- `crates/app/src/main.rs` — (a) signal-executor 直前に `RiskGate::check` 呼び出し / (b) 起動時に reconciler + balance_sync spawn / (c) 起動時 env 検証強化 / (d) Kill Switch 解除時刻判定の JST ヘルパ
- `crates/core/src/config.rs` — `RiskConfig { daily_loss_limit_pct, price_freshness_secs, kill_switch_release_jst_hour }` 追加 + `AppConfig.risk: Option<RiskConfig>`
- `config/default.toml` — `[risk]` セクション追加
- `.env.example` — `SLACK_WEBHOOK_URL` 必須条件明記

### 意図的に触らない
- `crates/executor/src/trader.rs` — Unified Trader 本体。PR-2 では Gate の呼び出し側を切るだけ
- `crates/market/src/bitflyer_private.rs` — 既存 6 メソッドで全て賄える
- `crates/notify/src/lib.rs` — NotifyEvent は全変種揃っている

---

## Task 1: `risk_halts` テーブル再作成

**Files:**
- Create: `migrations/20260416000001_risk_halts.sql`

- [ ] **Step 1: 失敗する migration smoke テストを書く**

現行テストスイートに `migrations` のロード確認がある。`crates/db/tests/migration_test.rs` (存在しなければ新規) に以下を追加。

```rust
// crates/db/tests/migration_test.rs
#[sqlx::test(migrations = "../../migrations")]
async fn risk_halts_table_exists(pool: sqlx::PgPool) -> sqlx::Result<()> {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_name = 'risk_halts'
        )",
    )
    .fetch_one(&pool)
    .await?;
    assert!(row.0, "risk_halts table should exist after migrations");
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn risk_halts_active_partial_index_exists(pool: sqlx::PgPool) -> sqlx::Result<()> {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE indexname = 'risk_halts_account_active'
        )",
    )
    .fetch_one(&pool)
    .await?;
    assert!(row.0);
    Ok(())
}
```

- [ ] **Step 2: テストを走らせて失敗することを確認**

```bash
DATABASE_URL=postgres://auto-trader:auto-trader@localhost:15432/auto_trader \
    cargo test -p auto-trader-db --test migration_test
```

Expected: FAIL (`risk_halts` テーブルが存在しない)

- [ ] **Step 3: migration を書く**

```sql
-- migrations/20260416000001_risk_halts.sql
-- Kill Switch 発動記録。PR-1 の unified_rewrite で drop したテーブルを
-- RiskGate 実装に合わせて再作成。trading_accounts FK に合わせる。
BEGIN;

CREATE TABLE IF NOT EXISTS risk_halts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id UUID NOT NULL REFERENCES trading_accounts(id) ON DELETE RESTRICT,
    -- Kill Switch 発動理由。将来 reason を増やしても文字列で後方互換。
    reason TEXT NOT NULL,
    -- 発動時点のスナップショット (観測可能性のため保存)。
    daily_loss NUMERIC NOT NULL,
    loss_limit NUMERIC NOT NULL,
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- UTC で保存。Kill Switch 解除判定時に NOW() >= halted_until で判定。
    halted_until TIMESTAMPTZ NOT NULL,
    released_at TIMESTAMPTZ,
    CONSTRAINT risk_halts_halt_after_trigger
        CHECK (halted_until > triggered_at)
);

-- アクティブな halt (未解除) を高速に引けるように partial index。
-- released_at IS NULL かつ halted_until > NOW() な行を引く想定。
CREATE INDEX IF NOT EXISTS risk_halts_account_active
    ON risk_halts (account_id, halted_until DESC)
    WHERE released_at IS NULL;

COMMIT;
```

- [ ] **Step 4: テストを走らせて通ることを確認**

```bash
DATABASE_URL=postgres://auto-trader:auto-trader@localhost:15432/auto_trader \
    cargo test -p auto-trader-db --test migration_test
```

Expected: PASS (2/2)

- [ ] **Step 5: コミット**

```bash
git add migrations/20260416000001_risk_halts.sql crates/db/tests/migration_test.rs
git commit -m "feat(db): re-add risk_halts table for RiskGate kill switch"
```

---

## Task 2: `risk_halts` DB アクセス層

**Files:**
- Create: `crates/db/src/risk_halts.rs`
- Modify: `crates/db/src/lib.rs` (`pub mod risk_halts;` 追加)

- [ ] **Step 1: 失敗するテストを書く**

```rust
// crates/db/src/risk_halts.rs の末尾
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use sqlx::PgPool;

    async fn seed_account(pool: &PgPool) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                                             initial_balance, current_balance, leverage, currency)
             VALUES ($1, 'test', 'paper', 'bitflyer_cfd', 'donchian_trend_v1',
                     30000, 30000, 2, 'JPY')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_and_fetch_active_halt(pool: PgPool) {
        let account_id = seed_account(&pool).await;
        let halted_until = chrono::Utc::now() + chrono::Duration::hours(24);
        insert_halt(
            &pool,
            account_id,
            "daily_loss_limit_exceeded",
            dec!(-1600),
            dec!(-1500),
            halted_until,
        )
        .await
        .unwrap();

        let active = active_halt_for_account(&pool, account_id).await.unwrap();
        assert!(active.is_some());
        let halt = active.unwrap();
        assert_eq!(halt.reason, "daily_loss_limit_exceeded");
        assert_eq!(halt.daily_loss, dec!(-1600));
        assert_eq!(halt.loss_limit, dec!(-1500));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn released_halt_not_returned_as_active(pool: PgPool) {
        let account_id = seed_account(&pool).await;
        let halted_until = chrono::Utc::now() + chrono::Duration::hours(24);
        let halt_id = insert_halt(
            &pool,
            account_id,
            "daily_loss_limit_exceeded",
            dec!(-1600),
            dec!(-1500),
            halted_until,
        )
        .await
        .unwrap();
        release_halt(&pool, halt_id).await.unwrap();
        let active = active_halt_for_account(&pool, account_id).await.unwrap();
        assert!(active.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn expired_halt_not_returned_as_active(pool: PgPool) {
        let account_id = seed_account(&pool).await;
        // halted_until を過去にしたいので直接 INSERT。
        let past = chrono::Utc::now() - chrono::Duration::minutes(1);
        // CHECK constraint は halted_until > triggered_at なので triggered_at も過去に。
        sqlx::query(
            "INSERT INTO risk_halts
                 (account_id, reason, daily_loss, loss_limit, triggered_at, halted_until)
             VALUES ($1, 'test', 0, -1500, $2, $3)",
        )
        .bind(account_id)
        .bind(past - chrono::Duration::hours(1))
        .bind(past)
        .execute(&pool)
        .await
        .unwrap();
        let active = active_halt_for_account(&pool, account_id).await.unwrap();
        assert!(active.is_none(), "expired halt must not count as active");
    }
}
```

- [ ] **Step 2: `crates/db/src/lib.rs` にモジュール登録**

```rust
// crates/db/src/lib.rs (既存 pub mod 群に追記)
pub mod risk_halts;
```

- [ ] **Step 3: テストを走らせて失敗確認**

```bash
cargo test -p auto-trader-db risk_halts 2>&1 | tail
```

Expected: FAIL (未定義)

- [ ] **Step 4: 実装を書く**

```rust
// crates/db/src/risk_halts.rs
//! Kill Switch 発動レコード。RiskGate から insert/query される。

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RiskHalt {
    pub id: Uuid,
    pub account_id: Uuid,
    pub reason: String,
    pub daily_loss: Decimal,
    pub loss_limit: Decimal,
    pub triggered_at: DateTime<Utc>,
    pub halted_until: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}

/// Kill Switch 発動レコードを作成し、id を返す。
pub async fn insert_halt(
    pool: &PgPool,
    account_id: Uuid,
    reason: &str,
    daily_loss: Decimal,
    loss_limit: Decimal,
    halted_until: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO risk_halts
             (account_id, reason, daily_loss, loss_limit, halted_until)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
    )
    .bind(account_id)
    .bind(reason)
    .bind(daily_loss)
    .bind(loss_limit)
    .bind(halted_until)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// アクティブ (未解除 かつ halted_until > NOW) な halt を1件返す。
/// RiskGate はこれが Some なら新規エントリーを拒否する。
pub async fn active_halt_for_account(
    pool: &PgPool,
    account_id: Uuid,
) -> anyhow::Result<Option<RiskHalt>> {
    let halt = sqlx::query_as::<_, RiskHalt>(
        "SELECT id, account_id, reason, daily_loss, loss_limit,
                triggered_at, halted_until, released_at
         FROM risk_halts
         WHERE account_id = $1
           AND released_at IS NULL
           AND halted_until > NOW()
         ORDER BY triggered_at DESC
         LIMIT 1",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(halt)
}

/// halt を手動解除 (主に Kill Switch 自動解除時刻到達で RiskGate が呼ぶ)。
pub async fn release_halt(pool: &PgPool, halt_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("UPDATE risk_halts SET released_at = NOW() WHERE id = $1 AND released_at IS NULL")
        .bind(halt_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// JST 本日の「クローズ済み trade の pnl_amount 合計」。Kill Switch 判定で
/// `含み損益 + 本関数の戻り値 <= -(initial_balance * limit_pct)` を評価する。
pub async fn daily_realized_pnl_jst(
    pool: &PgPool,
    account_id: Uuid,
) -> anyhow::Result<Decimal> {
    // JST 0:00 〜 (UTC ではなく JST 境界) で集計。FixedOffset で切る。
    let pnl: Option<Decimal> = sqlx::query_scalar(
        "SELECT SUM(pnl_amount) FROM trades
         WHERE account_id = $1
           AND status = 'closed'
           AND exit_at >= date_trunc('day', NOW() AT TIME ZONE 'Asia/Tokyo')
                          AT TIME ZONE 'Asia/Tokyo'",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(pnl.unwrap_or(Decimal::ZERO))
}
```

- [ ] **Step 5: テストを走らせて通ることを確認**

```bash
cargo test -p auto-trader-db risk_halts 2>&1 | tail
```

Expected: PASS (3/3)

- [ ] **Step 6: コミット**

```bash
git add crates/db/src/risk_halts.rs crates/db/src/lib.rs
git commit -m "feat(db): risk_halts access layer (insert/active/release/daily_pnl)"
```

---

## Task 3: RiskGate 本体 (純ロジック + DB 統合)

**Files:**
- Create: `crates/executor/src/risk_gate.rs`
- Create: `crates/executor/tests/risk_gate_test.rs`
- Modify: `crates/executor/src/lib.rs` (`pub mod risk_gate;`)

- [ ] **Step 1: `RiskConfig` を core/config に追加**

```rust
// crates/core/src/config.rs の AppConfig に追加
#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    /// 日次損失上限 (初期残高比)。0.05 = 5%。
    pub daily_loss_limit_pct: Decimal,
    /// price tick 鮮度閾値 (秒)。これを超えた tick での発注は拒否。
    pub price_freshness_secs: u64,
    /// Kill Switch 自動解除時刻 (JST 時)。通常 0 (= 翌日 0:00 JST)。
    pub kill_switch_release_jst_hour: u32,
}

impl RiskConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.daily_loss_limit_pct <= Decimal::ZERO
            || self.daily_loss_limit_pct > Decimal::ONE
        {
            anyhow::bail!("[risk].daily_loss_limit_pct must be in (0, 1]");
        }
        if self.price_freshness_secs == 0 {
            anyhow::bail!("[risk].price_freshness_secs must be > 0");
        }
        if self.kill_switch_release_jst_hour > 23 {
            anyhow::bail!("[risk].kill_switch_release_jst_hour must be 0..=23");
        }
        Ok(())
    }
}

// AppConfig に pub risk: Option<RiskConfig>, 追加 + validate で呼ぶ
```

- [ ] **Step 2: `config/default.toml` に `[risk]` 追加**

```toml
# config/default.toml の末尾に追加
[risk]
daily_loss_limit_pct = 0.05
price_freshness_secs = 60
kill_switch_release_jst_hour = 0
```

- [ ] **Step 3: 失敗テストを書く**

```rust
// crates/executor/tests/risk_gate_test.rs
//! RiskGate の単体テスト。DB はインメモリの fake で代用。

use auto_trader_core::types::{Direction, Pair, Signal};
use auto_trader_executor::risk_gate::{
    GateDecision, RejectReason, RiskGate, RiskGateConfig,
};
use chrono::Utc;
use rust_decimal_macros::dec;

fn sample_signal() -> Signal {
    Signal {
        strategy_name: "donchian_trend_v1".into(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        stop_loss_pct: dec!(0.03),
        take_profit_pct: None,
        confidence: 0.9,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        max_hold_until: None,
    }
}

fn sample_config() -> RiskGateConfig {
    RiskGateConfig {
        daily_loss_limit_pct: dec!(0.05),
        price_freshness_secs: 60,
    }
}

#[test]
fn rejects_when_price_tick_is_stale() {
    let cfg = sample_config();
    let age_secs = 90u64; // > 60
    let decision = RiskGate::eval_price_freshness(&cfg, age_secs);
    match decision {
        GateDecision::Reject(RejectReason::PriceTickStale { age_secs: a }) => {
            assert_eq!(a, 90);
        }
        other => panic!("expected PriceTickStale, got {:?}", other),
    }
}

#[test]
fn passes_when_price_tick_is_fresh() {
    let cfg = sample_config();
    let decision = RiskGate::eval_price_freshness(&cfg, 10);
    assert!(matches!(decision, GateDecision::Pass));
}

#[test]
fn rejects_when_daily_loss_exceeds_limit() {
    let cfg = sample_config();
    let initial_balance = dec!(30000);
    let realized = dec!(-1400);
    let unrealized = dec!(-200);
    let decision = RiskGate::eval_kill_switch(&cfg, initial_balance, realized, unrealized);
    match decision {
        GateDecision::Reject(RejectReason::DailyLossLimitExceeded { loss, limit }) => {
            assert_eq!(loss, dec!(-1600));
            assert_eq!(limit, dec!(-1500));
        }
        other => panic!("expected DailyLossLimitExceeded, got {:?}", other),
    }
}

#[test]
fn passes_when_daily_loss_within_limit() {
    let cfg = sample_config();
    let initial_balance = dec!(30000);
    let realized = dec!(-500);
    let unrealized = dec!(-200);
    let decision = RiskGate::eval_kill_switch(&cfg, initial_balance, realized, unrealized);
    assert!(matches!(decision, GateDecision::Pass));
}

#[test]
fn rejects_on_exact_limit_breach() {
    // 境界値: ちょうど -limit に達した瞬間も reject する (strict)
    let cfg = sample_config();
    let initial_balance = dec!(30000);
    let realized = dec!(-1500);
    let unrealized = dec!(0);
    let decision = RiskGate::eval_kill_switch(&cfg, initial_balance, realized, unrealized);
    assert!(matches!(
        decision,
        GateDecision::Reject(RejectReason::DailyLossLimitExceeded { .. })
    ));
}
```

- [ ] **Step 4: テスト失敗確認**

```bash
cargo test -p auto-trader-executor --test risk_gate_test 2>&1 | tail
```

Expected: FAIL (未定義)

- [ ] **Step 5: RiskGate 本体を実装**

```rust
// crates/executor/src/risk_gate.rs
//! Signal → Executor の間の前段ガード。paper/live 共通。
//!
//! ## 責務
//! - 価格 tick が古すぎる (WS 切断中など) 場合は拒否
//! - 同一 account × strategy × pair で open/closing のトレードがあれば拒否
//! - Kill Switch (日次損失 >= limit) が発動中なら拒否
//! - 新規に Kill Switch 発動条件を満たしたら insert + Notifier 通知
//!
//! ## 非責務 (意図的に持たない)
//! - 実発注処理 (Trader の責務)
//! - ポジションサイズ計算 (PositionSizer の責務)
//! - 分散ロック (DB partial unique + RiskGate 事前チェックの二重化で十分)

use anyhow::Context;
use auto_trader_core::types::Signal;
use auto_trader_db::risk_halts;
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_notify::{KillSwitchTriggeredEvent, NotifyEvent, Notifier};
use chrono::{DateTime, Duration, FixedOffset, TimeZone, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RiskGateConfig {
    pub daily_loss_limit_pct: Decimal,
    pub price_freshness_secs: u64,
}

#[derive(Debug)]
pub enum GateDecision {
    Pass,
    Reject(RejectReason),
}

#[derive(Debug, Clone)]
pub enum RejectReason {
    DailyLossLimitExceeded { loss: Decimal, limit: Decimal },
    PriceTickStale { age_secs: u64 },
    DuplicatePosition { existing_trade_id: Uuid },
    KillSwitchActive { until: DateTime<Utc> },
}

impl RejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DailyLossLimitExceeded { .. } => "daily_loss_limit_exceeded",
            Self::PriceTickStale { .. } => "price_tick_stale",
            Self::DuplicatePosition { .. } => "duplicate_position",
            Self::KillSwitchActive { .. } => "kill_switch_active",
        }
    }
}

pub struct RiskGate {
    pool: PgPool,
    notifier: Arc<Notifier>,
    config: RiskGateConfig,
    /// Kill Switch 自動解除時刻 (JST 時)。main.rs から渡す。
    release_jst_hour: u32,
}

impl RiskGate {
    pub fn new(
        pool: PgPool,
        notifier: Arc<Notifier>,
        config: RiskGateConfig,
        release_jst_hour: u32,
    ) -> Self {
        Self {
            pool,
            notifier,
            config,
            release_jst_hour,
        }
    }

    /// 全チェックを一気通貫で評価。拒否理由があれば返す。
    ///
    /// `last_tick_age_secs`: 呼び出し側が PriceStore から取得した
    /// 最新 tick の age を秒で渡す。tick 自体が存在しない場合は
    /// `u64::MAX` を渡せば自動的に stale と判定される。
    /// `current_unrealized`: 当該アカウントの未実現損益合計。DB から引く
    /// 計算量を減らすため呼び出し側が集計して渡す。
    pub async fn check(
        &self,
        signal: &Signal,
        account: &TradingAccount,
        last_tick_age_secs: u64,
        current_unrealized: Decimal,
    ) -> anyhow::Result<GateDecision> {
        // 1) price freshness
        if let GateDecision::Reject(r) =
            Self::eval_price_freshness(&self.config, last_tick_age_secs)
        {
            return Ok(GateDecision::Reject(r));
        }

        // 2) active halt (Kill Switch 発動中)
        if let Some(halt) =
            risk_halts::active_halt_for_account(&self.pool, account.id).await?
        {
            return Ok(GateDecision::Reject(RejectReason::KillSwitchActive {
                until: halt.halted_until,
            }));
        }

        // 3) duplicate position
        let existing = auto_trader_db::trades::find_open_for_strategy_pair(
            &self.pool,
            account.id,
            &signal.strategy_name,
            &signal.pair.0,
        )
        .await?;
        if let Some(trade_id) = existing {
            return Ok(GateDecision::Reject(RejectReason::DuplicatePosition {
                existing_trade_id: trade_id,
            }));
        }

        // 4) Kill Switch 条件 (今日の損失が閾値を超えているか)
        let realized =
            risk_halts::daily_realized_pnl_jst(&self.pool, account.id).await?;
        if let GateDecision::Reject(RejectReason::DailyLossLimitExceeded { loss, limit }) =
            Self::eval_kill_switch(
                &self.config,
                account.initial_balance,
                realized,
                current_unrealized,
            )
        {
            // 新規発動 — DB に halt 記録 + Slack 通知
            let halted_until = self.compute_halted_until(Utc::now());
            let _ = risk_halts::insert_halt(
                &self.pool,
                account.id,
                RejectReason::DailyLossLimitExceeded {
                    loss,
                    limit,
                }
                .as_str(),
                loss,
                limit,
                halted_until,
            )
            .await
            .context("risk_gate: insert_halt failed")?;
            // critical 通知は await する (fire-and-forget だと落ちたとき気付けない)
            let ev = NotifyEvent::KillSwitchTriggered(KillSwitchTriggeredEvent {
                account_name: account.name.clone(),
                daily_loss: loss,
                limit,
                halted_until,
            });
            if let Err(e) = self.notifier.send(ev).await {
                tracing::error!(
                    "risk_gate: Slack notify failed for KillSwitchTriggered (account={}): {e}",
                    account.name
                );
                // Notifier 内部で DB backstop を張っている想定 (spec 5.5)。
            }
            return Ok(GateDecision::Reject(RejectReason::DailyLossLimitExceeded {
                loss,
                limit,
            }));
        }

        Ok(GateDecision::Pass)
    }

    /// 純関数: price tick 鮮度のみ評価。テスタビリティのため分離。
    pub fn eval_price_freshness(cfg: &RiskGateConfig, age_secs: u64) -> GateDecision {
        if age_secs > cfg.price_freshness_secs {
            GateDecision::Reject(RejectReason::PriceTickStale { age_secs })
        } else {
            GateDecision::Pass
        }
    }

    /// 純関数: Kill Switch 発動条件のみ評価。
    ///
    /// `total_pnl = realized + unrealized` が `-(initial_balance * limit_pct)`
    /// 以下 (= 等しい or より損失) なら発動。境界値でも発動 (strict <=)。
    pub fn eval_kill_switch(
        cfg: &RiskGateConfig,
        initial_balance: Decimal,
        realized: Decimal,
        unrealized: Decimal,
    ) -> GateDecision {
        let limit_abs = initial_balance * cfg.daily_loss_limit_pct;
        let loss_limit = -limit_abs;
        let total = realized + unrealized;
        if total <= loss_limit {
            GateDecision::Reject(RejectReason::DailyLossLimitExceeded {
                loss: total,
                limit: loss_limit,
            })
        } else {
            GateDecision::Pass
        }
    }

    /// 現在時刻から「次の JST `release_jst_hour` 時」を返す。
    /// 例: release_jst_hour=0 なら翌日 JST 0:00 (= UTC 15:00)。
    /// 既に今日の該当時刻を過ぎていれば翌日、未来ならその時刻を返す。
    fn compute_halted_until(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let jst: FixedOffset = FixedOffset::east_opt(9 * 3600)
            .expect("9-hour offset is always valid");
        let jst_now = now.with_timezone(&jst);
        let today_release = jst
            .with_ymd_and_hms(
                jst_now.date_naive().and_hms_opt(0, 0, 0).unwrap().date(),
                self.release_jst_hour,
                0,
                0,
            )
            .single()
            .expect("release hour 0..=23 is always valid");
        let target = if today_release > jst_now {
            today_release
        } else {
            today_release + Duration::days(1)
        };
        target.with_timezone(&Utc)
    }
}
```

- [ ] **Step 6: `crates/db/src/trades.rs` に `find_open_for_strategy_pair` 追加**

既存の `list_open_by_account` があれば拡張、なければ新規。

```rust
// crates/db/src/trades.rs に追加
/// 指定 account × strategy × pair で status IN ('open', 'closing') な
/// trade を1件返す。RiskGate の duplicate position check で使う。
/// DB の partial unique index (migration で後続追加) と二重化して保険をかける。
pub async fn find_open_for_strategy_pair(
    pool: &PgPool,
    account_id: Uuid,
    strategy_name: &str,
    pair: &str,
) -> anyhow::Result<Option<Uuid>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM trades
         WHERE account_id = $1 AND strategy_name = $2 AND pair = $3
           AND status IN ('open', 'closing')
         LIMIT 1",
    )
    .bind(account_id)
    .bind(strategy_name)
    .bind(pair)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}
```

- [ ] **Step 7: `crates/executor/src/lib.rs` に module 追加**

```rust
// crates/executor/src/lib.rs 末尾付近
pub mod risk_gate;
```

- [ ] **Step 8: テスト通ることを確認**

```bash
cargo test -p auto-trader-executor --test risk_gate_test 2>&1 | tail
cargo test -p auto-trader-db 2>&1 | tail
```

Expected: PASS

- [ ] **Step 9: コミット**

```bash
git add crates/executor/src/risk_gate.rs crates/executor/src/lib.rs \
        crates/executor/tests/risk_gate_test.rs \
        crates/db/src/trades.rs \
        crates/core/src/config.rs config/default.toml
git commit -m "feat(executor): RiskGate (kill switch + price freshness + duplicate ban)"
```

---

## Task 4: duplicate position 用の DB partial unique index

RiskGate の事前チェックだけではレース条件で潜り抜ける可能性がある (2 signal が同時に発火)。DB レイヤで押さえる。

**Files:**
- Modify: `migrations/20260416000001_risk_halts.sql` (Task 1 で書いたファイルの末尾に追記)

- [ ] **Step 1: 失敗テストを書く**

```rust
// crates/db/tests/migration_test.rs に追加
#[sqlx::test(migrations = "../../migrations")]
async fn trades_one_active_per_strategy_pair_unique_exists(pool: sqlx::PgPool) {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE indexname = 'trades_one_active_per_strategy_pair'
        )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(row.0, "partial unique index must exist");
}
```

- [ ] **Step 2: テスト失敗確認**

```bash
cargo test -p auto-trader-db --test migration_test trades_one 2>&1 | tail
```

Expected: FAIL

- [ ] **Step 3: migration 末尾に追加**

```sql
-- migrations/20260416000001_risk_halts.sql の COMMIT; 直前に追記

-- 二重発注防止: 同一 account × strategy × pair で open/closing は1件まで。
-- RiskGate の pre-check と二重化。レースで潜り抜けた場合は DB が拒否する。
CREATE UNIQUE INDEX IF NOT EXISTS trades_one_active_per_strategy_pair
    ON trades (account_id, strategy_name, pair)
    WHERE status IN ('open', 'closing');
```

- [ ] **Step 4: 既存 DB に apply するためテスト DB は `sqlx::test` が自動でやるので問題なし。ローカル dev DB は手動 reset が必要**

```bash
# dev DB 用 (必要時)
DATABASE_URL=... cargo sqlx migrate run
```

- [ ] **Step 5: テストを走らせて通ることを確認**

```bash
cargo test -p auto-trader-db --test migration_test 2>&1 | tail
```

Expected: PASS (3/3)

- [ ] **Step 6: コミット**

```bash
git add migrations/20260416000001_risk_halts.sql crates/db/tests/migration_test.rs
git commit -m "feat(db): partial unique index for one active trade per strategy+pair"
```

---

## Task 5: RiskGate を main.rs の signal executor に配線

**Files:**
- Modify: `crates/app/src/main.rs` (signal-executor 箇所、既存 1111-1260 行あたり)

- [ ] **Step 1: RiskGate のインスタンス化 (startup block)**

`shared_position_sizer` の構築付近に以下を追加:

```rust
// crates/app/src/main.rs: shared_position_sizer の構築直後 (line ~541 付近)
let risk_config = config.risk.clone().unwrap_or_else(|| {
    // デフォルト値: [risk] セクションが未指定でも動く safe-default。
    // production config では必ず明示することを想定。
    auto_trader_core::config::RiskConfig {
        daily_loss_limit_pct: rust_decimal::Decimal::new(5, 2), // 0.05
        price_freshness_secs: 60,
        kill_switch_release_jst_hour: 0,
    }
});
let risk_gate = Arc::new(auto_trader_executor::risk_gate::RiskGate::new(
    pool.clone(),
    notifier.clone(),
    auto_trader_executor::risk_gate::RiskGateConfig {
        daily_loss_limit_pct: risk_config.daily_loss_limit_pct,
        price_freshness_secs: risk_config.price_freshness_secs,
    },
    risk_config.kill_switch_release_jst_hour,
));
```

- [ ] **Step 2: signal executor ループに clone + check を差し込む**

`let executor_position_sizer = shared_position_sizer.clone();` の直後に:

```rust
let executor_risk_gate = risk_gate.clone();
let executor_price_store = price_store.clone();  // もし既に別名で clone 済みなら再利用
```

signal 毎の処理ループ内 (既存 `let dry_run = pac.account_type == "paper" || executor_live_forces_dry_run;` の直後) に:

```rust
// RiskGate 事前チェック。live/paper 両方に適用。
let last_tick_age = executor_price_store
    .last_tick_age(&signal.pair)
    .unwrap_or(u64::MAX);
// 未実現損益は open トレードから集計。数が少ないのでベタ SQL で OK。
let unrealized = match auto_trader_db::trades::sum_unrealized_pnl_for_account(
    &executor_pool,
    pac.id,
    &executor_price_store,
)
.await
{
    Ok(v) => v,
    Err(e) => {
        tracing::error!(
            "risk_gate: failed to compute unrealized for {}: {e}",
            pac.name
        );
        rust_decimal::Decimal::ZERO
    }
};
let decision = executor_risk_gate
    .check(&signal, &pac, last_tick_age, unrealized)
    .await;
match decision {
    Ok(auto_trader_executor::risk_gate::GateDecision::Pass) => {}
    Ok(auto_trader_executor::risk_gate::GateDecision::Reject(reason)) => {
        tracing::warn!(
            "risk_gate rejected signal for {}: {:?}",
            pac.name,
            reason
        );
        continue; // 次 signal へ
    }
    Err(e) => {
        tracing::error!(
            "risk_gate check errored for {}: {e}; failing closed (skip)",
            pac.name
        );
        continue;
    }
}
```

- [ ] **Step 3: `PriceStore::last_tick_age` を追加**

`crates/market/src/price_store.rs` に:

```rust
/// 指定 pair の最新 tick が何秒前に受信されたか。pair が未観測なら None。
pub fn last_tick_age(&self, pair: &Pair) -> Option<u64> {
    let map = self.inner.read().unwrap();
    map.get(pair).map(|snap| {
        let age = chrono::Utc::now() - snap.timestamp;
        age.num_seconds().max(0) as u64
    })
}
```

対応する単体テスト:

```rust
// crates/market/src/price_store.rs の tests モジュールに追加
#[test]
fn last_tick_age_returns_zero_for_just_inserted_tick() {
    let store = PriceStore::new();
    let pair = Pair::new("FX_BTC_JPY");
    store.update(&pair, dec!(1000), dec!(1001), Utc::now());
    let age = store.last_tick_age(&pair).expect("should exist");
    assert!(age <= 1, "just-inserted tick should have age <= 1s");
}

#[test]
fn last_tick_age_returns_none_for_unknown_pair() {
    let store = PriceStore::new();
    let pair = Pair::new("UNKNOWN");
    assert!(store.last_tick_age(&pair).is_none());
}
```

- [ ] **Step 4: `sum_unrealized_pnl_for_account` を trades.rs に追加**

```rust
// crates/db/src/trades.rs に追加
/// 指定アカウントの open トレードの未実現損益合計。PriceStore の現在価格で評価。
/// RiskGate の Kill Switch 判定に使う。tick が無い pair はスキップ (ゼロ扱い)。
pub async fn sum_unrealized_pnl_for_account(
    pool: &PgPool,
    account_id: Uuid,
    price_store: &auto_trader_market::price_store::PriceStore,
) -> anyhow::Result<Decimal> {
    let rows: Vec<(String, String, Decimal, Decimal, i32)> = sqlx::query_as(
        "SELECT pair, direction, entry_price, quantity, 0
         FROM trades
         WHERE account_id = $1 AND status IN ('open', 'closing')",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    let mut total = Decimal::ZERO;
    for (pair, direction, entry, qty, _) in rows {
        let pair_obj = auto_trader_core::types::Pair::new(&pair);
        let Some(current) = price_store.mid(&pair_obj) else {
            continue;
        };
        let pnl = match direction.as_str() {
            "long" => (current - entry) * qty,
            "short" => (entry - current) * qty,
            _ => continue,
        };
        total += pnl;
    }
    Ok(total)
}
```

PriceStore に `mid` メソッドが無い場合は追加:

```rust
// crates/market/src/price_store.rs
pub fn mid(&self, pair: &Pair) -> Option<Decimal> {
    let map = self.inner.read().unwrap();
    map.get(pair).map(|s| (s.bid + s.ask) / Decimal::TWO)
}
```

(`Decimal::TWO` が無い場合は `Decimal::from(2)`。)

- [ ] **Step 5: build + 既存テストが通ることを確認**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail
DATABASE_URL=postgres://auto-trader:auto-trader@localhost:15432/auto_trader \
    cargo test --workspace 2>&1 | grep -E "test result:|FAIL" | tail
```

Expected: clippy 0 warnings、全テスト pass

- [ ] **Step 6: コミット**

```bash
git add crates/app/src/main.rs crates/market/src/price_store.rs crates/db/src/trades.rs
git commit -m "feat(app): wire RiskGate into signal executor (fail-closed on error)"
```

---

## Task 6: Reconciler タスク (live 起動時 + 定期実行)

**Files:**
- Create: `crates/app/src/tasks/mod.rs`
- Create: `crates/app/src/tasks/reconciler.rs`
- Create: `crates/app/tests/reconciler_test.rs`
- Modify: `crates/app/src/main.rs` (`mod tasks;` + spawn)

- [ ] **Step 1: 純粋差分関数の失敗テストを書く**

```rust
// crates/app/tests/reconciler_test.rs
use auto_trader::tasks::reconciler::{ReconcileDiff, compute_diff, DbOpen, ExchangeOpen};
use rust_decimal_macros::dec;
use uuid::Uuid;

#[test]
fn no_diff_when_db_and_exchange_match() {
    let trade_id = Uuid::new_v4();
    let db = vec![DbOpen {
        trade_id,
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.01),
    }];
    let exch = vec![ExchangeOpen {
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.01),
    }];
    let diff = compute_diff(&db, &exch);
    assert!(diff.db_orphan.is_empty());
    assert!(diff.exchange_orphan.is_empty());
    assert!(diff.quantity_mismatch.is_empty());
}

#[test]
fn detects_db_orphan_when_exchange_lacks_position() {
    let trade_id = Uuid::new_v4();
    let db = vec![DbOpen {
        trade_id,
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.01),
    }];
    let exch: Vec<ExchangeOpen> = vec![];
    let diff = compute_diff(&db, &exch);
    assert_eq!(diff.db_orphan, vec![trade_id]);
}

#[test]
fn detects_exchange_orphan_when_db_lacks_position() {
    let db: Vec<DbOpen> = vec![];
    let exch = vec![ExchangeOpen {
        pair: "FX_BTC_JPY".into(),
        direction: "short".into(),
        quantity: dec!(0.02),
    }];
    let diff = compute_diff(&db, &exch);
    assert_eq!(diff.exchange_orphan.len(), 1);
    assert_eq!(diff.exchange_orphan[0].pair, "FX_BTC_JPY");
    assert_eq!(diff.exchange_orphan[0].quantity, dec!(0.02));
}

#[test]
fn detects_quantity_mismatch_same_direction() {
    let trade_id = Uuid::new_v4();
    let db = vec![DbOpen {
        trade_id,
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.01),
    }];
    let exch = vec![ExchangeOpen {
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.02),
    }];
    let diff = compute_diff(&db, &exch);
    assert_eq!(diff.quantity_mismatch.len(), 1);
    let m = &diff.quantity_mismatch[0];
    assert_eq!(m.trade_id, trade_id);
    assert_eq!(m.db_qty, dec!(0.01));
    assert_eq!(m.exchange_qty, dec!(0.02));
}
```

- [ ] **Step 2: tasks モジュール枠を作る**

```rust
// crates/app/src/tasks/mod.rs
pub mod balance_sync;
pub mod reconciler;
```

`crates/app/src/main.rs` 先頭付近に `mod tasks;` を追加。バイナリのみで lib が無い場合は、`tasks` モジュールをテストで参照できるよう **`crates/app/src/lib.rs`** を作成 (既存で無ければ) し以下を置く:

```rust
// crates/app/src/lib.rs (新規。既存 main.rs と両立する)
pub mod tasks;
// 他の再エクスポートは必要時のみ。
```

`crates/app/Cargo.toml` に lib target を追加:

```toml
[lib]
name = "auto_trader"
path = "src/lib.rs"

[[bin]]
name = "auto-trader"
path = "src/main.rs"
```

main.rs の上部で `use auto_trader::tasks` できるようになる。

- [ ] **Step 3: テスト失敗確認**

```bash
cargo test -p auto-trader --test reconciler_test 2>&1 | tail
```

Expected: FAIL (未定義)

- [ ] **Step 4: reconciler 実装**

```rust
// crates/app/src/tasks/reconciler.rs
//! live 口座の DB open トレードと取引所建玉の差分検出。
//! 自動修復はしない (手動対処)。差分があれば Slack 通知のみ。

use auto_trader_db::trading_accounts::{self, TradingAccount};
use auto_trader_market::bitflyer_private::BitflyerPrivateApi;
use auto_trader_notify::{NotifyEvent, Notifier, StartupReconciliationDiffEvent};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct DbOpen {
    pub trade_id: Uuid,
    pub pair: String,
    pub direction: String, // "long" | "short"
    pub quantity: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExchangeOpen {
    pub pair: String,
    pub direction: String,
    pub quantity: Decimal,
}

#[derive(Debug, Clone)]
pub struct QuantityMismatch {
    pub trade_id: Uuid,
    pub pair: String,
    pub db_qty: Decimal,
    pub exchange_qty: Decimal,
}

#[derive(Debug, Default)]
pub struct ReconcileDiff {
    pub db_orphan: Vec<Uuid>,
    pub exchange_orphan: Vec<ExchangeOpen>,
    pub quantity_mismatch: Vec<QuantityMismatch>,
}

/// 純関数: DB と取引所の建玉を pair+direction でキー集計し差分を出す。
pub fn compute_diff(db: &[DbOpen], exch: &[ExchangeOpen]) -> ReconcileDiff {
    let mut diff = ReconcileDiff::default();
    use std::collections::HashMap;
    // 集計: (pair, direction) → (qty_sum, trade_ids)
    let mut db_by_key: HashMap<(String, String), (Decimal, Vec<Uuid>)> = HashMap::new();
    for o in db {
        let k = (o.pair.clone(), o.direction.clone());
        let e = db_by_key.entry(k).or_insert((Decimal::ZERO, Vec::new()));
        e.0 += o.quantity;
        e.1.push(o.trade_id);
    }
    let mut exch_by_key: HashMap<(String, String), Decimal> = HashMap::new();
    for o in exch {
        *exch_by_key.entry((o.pair.clone(), o.direction.clone())).or_insert(Decimal::ZERO) +=
            o.quantity;
    }

    for (key, (db_qty, trade_ids)) in &db_by_key {
        match exch_by_key.get(key) {
            None => diff.db_orphan.extend(trade_ids),
            Some(ex_qty) => {
                if db_qty != ex_qty {
                    // 複数 trade が同一 (pair,direction) にいる場合、どの trade が
                    // drift の原因か特定できないので代表として先頭 trade_id を載せる。
                    diff.quantity_mismatch.push(QuantityMismatch {
                        trade_id: trade_ids[0],
                        pair: key.0.clone(),
                        db_qty: *db_qty,
                        exchange_qty: *ex_qty,
                    });
                }
            }
        }
    }
    for (key, ex_qty) in &exch_by_key {
        if !db_by_key.contains_key(key) {
            diff.exchange_orphan.push(ExchangeOpen {
                pair: key.0.clone(),
                direction: key.1.clone(),
                quantity: *ex_qty,
            });
        }
    }
    diff
}

/// live アカウント1件の reconciliation を1サイクル実行。
pub async fn reconcile_account(
    pool: &PgPool,
    api: &BitflyerPrivateApi,
    notifier: &Notifier,
    account: &TradingAccount,
    product_code: &str,
) -> anyhow::Result<()> {
    let db_rows: Vec<(Uuid, String, String, Decimal)> = sqlx::query_as(
        "SELECT id, pair, direction, quantity FROM trades
         WHERE account_id = $1 AND status IN ('open', 'closing')",
    )
    .bind(account.id)
    .fetch_all(pool)
    .await?;
    let db_opens: Vec<DbOpen> = db_rows
        .into_iter()
        .map(|(id, pair, direction, qty)| DbOpen {
            trade_id: id,
            pair,
            direction,
            quantity: qty,
        })
        .collect();

    let positions = api.get_positions(product_code).await?;
    let exch_opens: Vec<ExchangeOpen> = positions
        .iter()
        .map(|p| ExchangeOpen {
            pair: product_code.to_string(),
            direction: if p.side.eq_ignore_ascii_case("BUY") {
                "long".into()
            } else {
                "short".into()
            },
            quantity: p.size,
        })
        .collect();

    let diff = compute_diff(&db_opens, &exch_opens);
    if diff.db_orphan.is_empty()
        && diff.exchange_orphan.is_empty()
        && diff.quantity_mismatch.is_empty()
    {
        return Ok(());
    }

    tracing::warn!(
        "reconciler drift for {}: db_orphan={} exch_orphan={} qty_mismatch={}",
        account.name,
        diff.db_orphan.len(),
        diff.exchange_orphan.len(),
        diff.quantity_mismatch.len(),
    );
    let ev = NotifyEvent::StartupReconciliationDiff(StartupReconciliationDiffEvent {
        account_name: account.name.clone(),
        db_orphan: diff.db_orphan,
        exchange_orphan_count: diff.exchange_orphan.len(),
        quantity_mismatch_count: diff.quantity_mismatch.len(),
    });
    if let Err(e) = notifier.send(ev).await {
        tracing::error!("reconciler notify failed for {}: {e}", account.name);
    }
    Ok(())
}

/// バックグラウンドタスク: 起動時 1 回 + `interval_secs` 毎に全 live アカウントを reconcile。
pub async fn run_reconciler_loop(
    pool: PgPool,
    api: Arc<BitflyerPrivateApi>,
    notifier: Arc<Notifier>,
    product_code: String,
    interval_secs: u64,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        ticker.tick().await;
        let accounts = match trading_accounts::list_all(&pool).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("reconciler: list_all failed: {e}");
                continue;
            }
        };
        for acc in &accounts {
            if acc.account_type != "live" {
                continue;
            }
            if let Err(e) =
                reconcile_account(&pool, &api, &notifier, acc, &product_code).await
            {
                tracing::error!("reconciler: account {} errored: {e}", acc.name);
            }
        }
    }
}
```

- [ ] **Step 5: `StartupReconciliationDiffEvent` のフィールド構造を notify crate で確認/調整**

現行定義が `orphan_db: Vec<Uuid>, orphan_exchange: Vec<String>` 風になっている場合は、本 plan の呼び出しに合わせて以下を使う。不一致ならどちらかを合わせる (notify crate を直す方が他の呼び出し箇所が無ければ楽):

```rust
// crates/notify/src/lib.rs 既存定義を以下に合わせる (必要時)
pub struct StartupReconciliationDiffEvent {
    pub account_name: String,
    pub db_orphan: Vec<uuid::Uuid>,
    pub exchange_orphan_count: usize,
    pub quantity_mismatch_count: usize,
}
```

テンプレ文字列 (Slack メッセージ組み立て) も対応修正。

- [ ] **Step 6: テスト走らせて通ることを確認**

```bash
cargo test -p auto-trader --test reconciler_test 2>&1 | tail
```

Expected: PASS (4/4)

- [ ] **Step 7: main.rs で spawn する**

main.rs の executor_handle spawn 付近の後に:

```rust
// live reconciler: config.live.enabled かつ BitflyerPrivateApi が使える時のみ。
if let Some(live_cfg) = config.live.as_ref().filter(|l| l.enabled) {
    let recon_pool = pool.clone();
    let recon_api = bitflyer_api.clone();
    let recon_notifier = notifier.clone();
    let recon_interval = live_cfg.reconciler_interval_secs;
    let _recon_handle = tokio::spawn(async move {
        auto_trader::tasks::reconciler::run_reconciler_loop(
            recon_pool,
            recon_api,
            recon_notifier,
            "FX_BTC_JPY".to_string(),
            recon_interval,
        )
        .await;
    });
}
```

- [ ] **Step 8: clippy + build**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail
cargo check --workspace 2>&1 | tail
```

Expected: 0 warnings

- [ ] **Step 9: コミット**

```bash
git add crates/app/src/lib.rs crates/app/Cargo.toml \
        crates/app/src/tasks/mod.rs crates/app/src/tasks/reconciler.rs \
        crates/app/tests/reconciler_test.rs crates/app/src/main.rs \
        crates/notify/src/lib.rs
git commit -m "feat(app): periodic reconciler for live DB↔exchange position drift"
```

---

## Task 7: BalanceSync タスク

**Files:**
- Create: `crates/app/src/tasks/balance_sync.rs`
- Create: `crates/app/tests/balance_sync_test.rs`
- Modify: `crates/app/src/main.rs` (spawn)
- Modify: `crates/db/src/trading_accounts.rs` (`update_balance` が既にあればそのまま)

- [ ] **Step 1: drift 判定の失敗テストを書く**

```rust
// crates/app/tests/balance_sync_test.rs
use auto_trader::tasks::balance_sync::{is_drift_over_threshold};
use rust_decimal_macros::dec;

#[test]
fn drift_over_one_percent_is_reported() {
    // db=30000, exchange=30400 → diff 400 / 30000 ≈ 1.33% > 1%
    assert!(is_drift_over_threshold(dec!(30000), dec!(30400), dec!(0.01)));
}

#[test]
fn drift_at_exactly_threshold_is_not_reported() {
    // db=30000, exchange=30300 → diff 1.0% (境界、等値は「下」扱いで not reported)
    assert!(!is_drift_over_threshold(dec!(30000), dec!(30300), dec!(0.01)));
}

#[test]
fn no_drift_when_values_equal() {
    assert!(!is_drift_over_threshold(dec!(30000), dec!(30000), dec!(0.01)));
}

#[test]
fn drift_works_with_negative_diff() {
    // db=30000, exchange=29500 → -500 / 30000 ≈ 1.67% > 1%
    assert!(is_drift_over_threshold(dec!(30000), dec!(29500), dec!(0.01)));
}

#[test]
fn zero_db_balance_never_triggers_drift_div_by_zero() {
    // 0 除算回避。exchange 側は何でも返せるが false 返すのが安全寄り。
    assert!(!is_drift_over_threshold(dec!(0), dec!(100), dec!(0.01)));
}
```

- [ ] **Step 2: テスト失敗確認**

```bash
cargo test -p auto-trader --test balance_sync_test 2>&1 | tail
```

Expected: FAIL

- [ ] **Step 3: 実装**

```rust
// crates/app/src/tasks/balance_sync.rs
//! live 口座の current_balance を定期的に bitFlyer から同期。
//! 差分が閾値を超えたら Slack 通知 (auto-update は常に行う)。

use auto_trader_db::trading_accounts::{self, TradingAccount};
use auto_trader_market::bitflyer_private::BitflyerPrivateApi;
use auto_trader_notify::{BalanceDriftEvent, NotifyEvent, Notifier};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

/// db と exchange 残高の差分が threshold (0.01 == 1%) を **厳密に超える** なら true。
/// `db_balance == 0` の場合は除算を避けて false (= 通知しない)。
pub fn is_drift_over_threshold(
    db_balance: Decimal,
    exchange_balance: Decimal,
    threshold: Decimal,
) -> bool {
    if db_balance.is_zero() {
        return false;
    }
    let diff = (exchange_balance - db_balance).abs();
    let ratio = diff / db_balance;
    ratio > threshold
}

pub async fn sync_account(
    pool: &PgPool,
    api: &BitflyerPrivateApi,
    notifier: &Notifier,
    account: &TradingAccount,
    drift_threshold: Decimal,
) -> anyhow::Result<()> {
    let collateral = api.get_collateral().await?;
    // bitFlyer の collateral は JPY。Decimal で持っている想定 (pr #41 で定義済み)。
    let exchange_balance = collateral.collateral; // フィールド名は crate 側合わせ
    if is_drift_over_threshold(account.current_balance, exchange_balance, drift_threshold) {
        let ev = NotifyEvent::BalanceDrift(BalanceDriftEvent {
            account_name: account.name.clone(),
            db_balance: account.current_balance,
            exchange_balance,
        });
        if let Err(e) = notifier.send(ev).await {
            tracing::error!("balance_sync notify failed for {}: {e}", account.name);
        }
    }
    // drift 有無に関わらず DB は常に真実値に寄せる。
    trading_accounts::update_balance(pool, account.id, exchange_balance).await?;
    Ok(())
}

pub async fn run_balance_sync_loop(
    pool: PgPool,
    api: Arc<BitflyerPrivateApi>,
    notifier: Arc<Notifier>,
    interval_secs: u64,
    drift_threshold: Decimal,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        ticker.tick().await;
        let accounts = match trading_accounts::list_all(&pool).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("balance_sync: list_all failed: {e}");
                continue;
            }
        };
        for acc in &accounts {
            if acc.account_type != "live" {
                continue;
            }
            if let Err(e) =
                sync_account(&pool, &api, &notifier, acc, drift_threshold).await
            {
                tracing::error!("balance_sync: account {} errored: {e}", acc.name);
            }
        }
    }
}
```

- [ ] **Step 4: `BalanceDriftEvent` フィールドが `account_name / db_balance / exchange_balance` になっていることを確認。不足ならば notify crate を修正**

- [ ] **Step 5: main.rs で spawn する**

Reconciler と同じ条件分岐の中で:

```rust
if let Some(live_cfg) = config.live.as_ref().filter(|l| l.enabled) {
    // ... reconciler spawn ...
    let bs_pool = pool.clone();
    let bs_api = bitflyer_api.clone();
    let bs_notifier = notifier.clone();
    let bs_interval = live_cfg.balance_sync_interval_secs;
    let drift_threshold = rust_decimal::Decimal::new(1, 2); // 0.01 = 1%
    let _bs_handle = tokio::spawn(async move {
        auto_trader::tasks::balance_sync::run_balance_sync_loop(
            bs_pool,
            bs_api,
            bs_notifier,
            bs_interval,
            drift_threshold,
        )
        .await;
    });
}
```

- [ ] **Step 6: 全テスト pass**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail
DATABASE_URL=postgres://auto-trader:auto-trader@localhost:15432/auto_trader \
    cargo test --workspace 2>&1 | grep -E "test result:|FAIL" | tail
```

Expected: PASS

- [ ] **Step 7: コミット**

```bash
git add crates/app/src/tasks/balance_sync.rs \
        crates/app/tests/balance_sync_test.rs crates/app/src/main.rs \
        crates/notify/src/lib.rs
git commit -m "feat(app): periodic balance_sync for live trading_accounts"
```

---

## Task 8: 起動時 env 検証の厳格化

現行 (PR-1 終了時) の main.rs は `[live].enabled=true` かつ live account が存在するときに fail-fast する。PR-2 ではさらに `SLACK_WEBHOOK_URL` 必須化を追加する。spec §6.3 相当。

**Files:**
- Modify: `crates/app/src/main.rs` (既存 live gate の直後)

- [ ] **Step 1: 失敗テストを書く (= 単体関数化した validate 関数でテスト)**

検証ロジックを純関数に切り出す:

```rust
// crates/app/src/lib.rs (または crates/app/src/startup.rs 新規) に追加
pub mod startup {
    use auto_trader_core::config::LiveConfig;
    use auto_trader_db::trading_accounts::TradingAccount;

    pub fn validate_startup(
        accounts: &[TradingAccount],
        live_cfg: Option<&LiveConfig>,
        slack_webhook_env: Option<&str>,
        bitflyer_key_env: Option<&str>,
        bitflyer_secret_env: Option<&str>,
    ) -> anyhow::Result<()> {
        let has_live = accounts.iter().any(|a| a.account_type == "live");
        let live_enabled = live_cfg.is_some_and(|l| l.enabled);
        let dry_run = live_cfg.is_some_and(|l| l.dry_run);

        if has_live && !live_enabled {
            anyhow::bail!(
                "refusing to start: account_type='live' row(s) present but [live].enabled is false"
            );
        }

        if live_enabled {
            // 実発注経路に入る設定なら Slack 必須 (dry_run 中でも観測できないと駄目)。
            if slack_webhook_env.unwrap_or("").is_empty() {
                anyhow::bail!(
                    "refusing to start: [live].enabled=true requires SLACK_WEBHOOK_URL"
                );
            }
            if !dry_run {
                // 実発注する気なら API key 必須。
                if bitflyer_key_env.unwrap_or("").is_empty()
                    || bitflyer_secret_env.unwrap_or("").is_empty()
                {
                    anyhow::bail!(
                        "refusing to start: [live].enabled=true with dry_run=false requires BITFLYER_API_KEY/SECRET"
                    );
                }
            }
        }
        Ok(())
    }
}
```

テスト:

```rust
// crates/app/tests/startup_test.rs
use auto_trader::startup::validate_startup;
use auto_trader_core::config::LiveConfig;
use auto_trader_db::trading_accounts::TradingAccount;
use rust_decimal_macros::dec;
use uuid::Uuid;

fn live_account() -> TradingAccount {
    TradingAccount {
        id: Uuid::new_v4(),
        name: "live1".into(),
        account_type: "live".into(),
        exchange: "bitflyer_cfd".into(),
        strategy: "donchian_trend_v1".into(),
        initial_balance: dec!(30000),
        current_balance: dec!(30000),
        leverage: dec!(2),
        currency: "JPY".into(),
        created_at: chrono::Utc::now(),
    }
}

fn live_cfg(enabled: bool, dry_run: bool) -> LiveConfig {
    LiveConfig {
        enabled,
        dry_run,
        execution_poll_interval_secs: 3,
        reconciler_interval_secs: 300,
        balance_sync_interval_secs: 300,
    }
}

#[test]
fn fails_when_live_account_exists_but_disabled() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(false, true)),
        Some("https://hook"),
        Some("k"),
        Some("s"),
    );
    assert!(r.is_err());
}

#[test]
fn fails_when_live_enabled_without_slack_webhook() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(true, true)),
        Some(""),
        Some("k"),
        Some("s"),
    );
    assert!(r.is_err());
}

#[test]
fn fails_when_real_trading_without_api_keys() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(true, false)),
        Some("https://hook"),
        Some(""),
        Some(""),
    );
    assert!(r.is_err());
}

#[test]
fn passes_when_live_dry_run_with_slack() {
    let r = validate_startup(
        &[live_account()],
        Some(&live_cfg(true, true)),
        Some("https://hook"),
        None,
        None,
    );
    assert!(r.is_ok());
}

#[test]
fn passes_with_only_paper_accounts() {
    let mut a = live_account();
    a.account_type = "paper".into();
    let r = validate_startup(&[a], None, None, None, None);
    assert!(r.is_ok());
}
```

- [ ] **Step 2: 失敗確認**

```bash
cargo test -p auto-trader --test startup_test 2>&1 | tail
```

Expected: FAIL (未定義)

- [ ] **Step 3: `startup.rs` を実装 + `main.rs` 既存 gate を置き換え**

main.rs の既存の inline live-gate ブロック (PR-1 で追加した部分) を `validate_startup` 呼び出しに差し替え:

```rust
// crates/app/src/main.rs 既存の inline gate を以下に置換
auto_trader::startup::validate_startup(
    &db_accounts,
    config.live.as_ref(),
    std::env::var("SLACK_WEBHOOK_URL").ok().as_deref(),
    std::env::var("BITFLYER_API_KEY").ok().as_deref(),
    std::env::var("BITFLYER_API_SECRET").ok().as_deref(),
)?;
```

- [ ] **Step 4: テスト pass**

```bash
cargo test -p auto-trader --test startup_test 2>&1 | tail
```

Expected: PASS (5/5)

- [ ] **Step 5: コミット**

```bash
git add crates/app/src/lib.rs crates/app/src/startup.rs \
        crates/app/tests/startup_test.rs crates/app/src/main.rs
git commit -m "feat(app): strict startup validation (Slack + API keys required for live)"
```

---

## Task 9: wiremock 統合テスト (Reconciler + BalanceSync)

Spec §8.2 の要件のうち、PR-2 で提供すべきシナリオ。

**Files:**
- Create: `crates/app/tests/live_integration_test.rs`
- Modify: `crates/app/Cargo.toml` (`[dev-dependencies]` に `wiremock = "0.6"`)

- [ ] **Step 1: Cargo.toml に wiremock 追加**

```toml
# crates/app/Cargo.toml
[dev-dependencies]
# 既存 dev-deps に追加
wiremock = "0.6"
```

- [ ] **Step 2: reconciler happy path テスト**

```rust
// crates/app/tests/live_integration_test.rs
use auto_trader_market::bitflyer_private::BitflyerPrivateApi;
use auto_trader_notify::Notifier;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn make_api(base_url: &str) -> BitflyerPrivateApi {
    BitflyerPrivateApi::new_with_base(
        base_url.to_string(),
        "test_key".into(),
        "test_secret".into(),
    )
}

#[sqlx::test(migrations = "../../migrations")]
async fn reconciler_reports_db_orphan_when_exchange_returns_empty(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getpositions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    // live アカウント + open trade を seed
    let account_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                                        initial_balance, current_balance, leverage, currency)
         VALUES ($1, 'live1', 'live', 'bitflyer_cfd', 'donchian_trend_v1',
                 30000, 30000, 2, 'JPY')",
    )
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    let trade_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO trades (id, account_id, strategy_name, pair, exchange, direction,
                             entry_price, quantity, leverage, stop_loss, entry_at, status)
         VALUES ($1, $2, 'donchian_trend_v1', 'FX_BTC_JPY', 'bitflyer_cfd', 'long',
                 5000000, 0.01, 2, 4800000, NOW(), 'open')",
    )
    .bind(trade_id)
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    let api = Arc::new(make_api(&server.uri()).await);
    let notifier = Arc::new(Notifier::new_disabled());
    let account = auto_trader_db::trading_accounts::get(&pool, account_id)
        .await
        .unwrap()
        .unwrap();

    auto_trader::tasks::reconciler::reconcile_account(
        &pool,
        &api,
        &notifier,
        &account,
        "FX_BTC_JPY",
    )
    .await
    .unwrap();
    // 成功 = panic 無し。通知は Notifier::new_disabled() なので no-op で抜ける。
    // (より厳密には MockNotifier を用意して呼び出し回数を assert するのが望ましいが、
    //  PR-2 内では純関数 compute_diff 側でカバレッジ担保する。)
}

#[sqlx::test(migrations = "../../migrations")]
async fn balance_sync_updates_current_balance_from_exchange(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me/getcollateral"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "collateral": 30500.0,
            "open_position_pnl": 0.0,
            "require_collateral": 0.0,
            "keep_rate": 0.0,
        })))
        .mount(&server)
        .await;

    let account_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                                        initial_balance, current_balance, leverage, currency)
         VALUES ($1, 'live1', 'live', 'bitflyer_cfd', 'donchian_trend_v1',
                 30000, 30000, 2, 'JPY')",
    )
    .bind(account_id)
    .execute(&pool)
    .await
    .unwrap();

    let api = make_api(&server.uri()).await;
    let notifier = Notifier::new_disabled();
    let account = auto_trader_db::trading_accounts::get(&pool, account_id)
        .await
        .unwrap()
        .unwrap();

    auto_trader::tasks::balance_sync::sync_account(
        &pool,
        &api,
        &notifier,
        &account,
        dec!(0.01),
    )
    .await
    .unwrap();

    let updated = auto_trader_db::trading_accounts::get(&pool, account_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.current_balance, dec!(30500));
}
```

- [ ] **Step 3: `Notifier::new_disabled` と `trading_accounts::get` の存在確認、必要なら追加**

```rust
// crates/notify/src/lib.rs
impl Notifier {
    /// SLACK_WEBHOOK_URL 未設定時と等価の Notifier を構築するテストヘルパ。
    pub fn new_disabled() -> Self {
        Self::new(None)
    }
}
```

```rust
// crates/db/src/trading_accounts.rs
pub async fn get(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<TradingAccount>> {
    let sql = format!(
        r#"SELECT {ACCOUNT_COLUMNS} FROM trading_accounts WHERE id = $1"#
    );
    let row = sqlx::query_as::<_, AccountRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(TradingAccount::from))
}
```

`BitflyerPrivateApi::new_with_base` も無ければ追加 (既存の `new` コンストラクタのテスト用バリアント):

```rust
// crates/market/src/bitflyer_private.rs
impl BitflyerPrivateApi {
    /// テスト用: wiremock のモックサーバに向ける。
    pub fn new_with_base(base_url: String, api_key: String, api_secret: String) -> Self {
        Self::new_internal(base_url, api_key, api_secret)
    }
}
```

既存コンストラクタの内部実装を参照し、同じ初期化を行う。

- [ ] **Step 4: テスト走らせて通ることを確認**

```bash
DATABASE_URL=postgres://auto-trader:auto-trader@localhost:15432/auto_trader \
    cargo test -p auto-trader --test live_integration_test 2>&1 | tail
```

Expected: PASS (2/2)

- [ ] **Step 5: コミット**

```bash
git add crates/app/Cargo.toml crates/app/tests/live_integration_test.rs \
        crates/notify/src/lib.rs crates/db/src/trading_accounts.rs \
        crates/market/src/bitflyer_private.rs
git commit -m "test(app): wiremock integration tests for reconciler + balance_sync"
```

---

## Task 10: 仕上げ — code-review スキル + PR 作成

- [ ] **Step 1: `superpowers:verification-before-completion` で最終検証**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
DATABASE_URL=postgres://auto-trader:auto-trader@localhost:15432/auto_trader \
    cargo test --workspace
```

3 コマンド全て exit 0 を目視確認。

- [ ] **Step 2: `simplify` スキル実行**

変更コードの重複/過剰抽象を整理。特に以下を確認:
- RiskGate の pure-fn と DB 呼び出し層の分離が適切か
- Reconciler の compute_diff と reconcile_account の責務分離が適切か
- main.rs の spawn ブロックがベタ書きで膨らんでいないか → 膨らんでいれば `fn spawn_live_tasks(...)` に切り出す

- [ ] **Step 3: `code-review` スキル実行 (codex round ループ)**

CLAUDE.md の code-review フロー通りに self-review → codex round → PR 作成 → Copilot round ループ。

- [ ] **Step 4: PR description テンプレ**

```markdown
## 概要 (PR-2/3)

PR-1 で導入した Unified Trader に対し、live 発注に耐える前段ガード /
起動時・定期リコンシリエーション / 残高同期を追加する。

## 含まれる変更
- RiskGate: Kill Switch (日次損失 5%), price tick freshness (60s), duplicate position ban
- DB: `risk_halts` テーブル再作成, `trades_one_active_per_strategy_pair` partial unique index
- Reconciler: live アカウントの DB vs bitFlyer 建玉差分検出 (5分毎 + 起動時)
- BalanceSync: live アカウントの `getcollateral` 定期同期 (5分毎、drift 1% 超で通知)
- 起動時 env 検証: `[live].enabled=true` で SLACK_WEBHOOK_URL 必須、`dry_run=false` で API key 必須
- wiremock 統合テスト (reconciler + balance_sync)

## 含まれない変更
- UI (PR-3 対応)
- 実運用デプロイ (PR-3 まで完了後)

## Test Plan
- [ ] `cargo test --workspace` 全 pass
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 0 warnings
- [ ] wiremock ベース統合テスト pass
- [ ] manual: `LIVE_DRY_RUN=true` で起動し、live 口座を作成 → reconciler/balance_sync がログに出ることを確認
```

- [ ] **Step 5: PR 作成後、`superpowers:finishing-a-development-branch` で merge/close**

---

## Self-Review チェック

- **Spec coverage (§5.3/5.7/5.8/5.9/5.10/6.3):**
  - 5.3 RiskGate → Task 3, 5
  - 5.7 DB migration (pending/inconsistent) → **意図的に除外** (PR-1 の同期的 fill により不要化)。コメントで記録。
  - 5.7 risk_halts + partial unique index → Task 1, 4
  - 5.8 Reconciler → Task 6
  - 5.9 BalanceSync → Task 7
  - 5.10 Fee model split → **PR-1 時点で対応済** (main.rs:1517 `if pac.account_type != "paper"`)。追加作業なし。
  - 6.3 起動時バリデーション → Task 8
  - 8.2 統合テスト → Task 9
  - **Gap: WebSocketDisconnected 通知**: `price_freshness_secs` で症状は検出できるが、専用の WS-disconnect 監視ループは無い → **defer to PR-3** (UI 側のバナーと合わせて対応する方が一貫性がある)

- **Placeholder scan:**
  - なし (全ステップにコード/コマンド/期待値を明示)

- **Type consistency:**
  - `TradingAccount` のフィールド (`account_type: String`, `current_balance: Decimal` など) は `crates/db/src/trading_accounts.rs` の既存定義に一致
  - `Signal` / `Pair` / `Direction` は PR-1 で確定した定義を流用
  - `BitflyerPrivateApi` の `get_positions` / `get_collateral` 戻り値型は PR #41 定義を前提 (`ExchangePosition`, `Collateral`)。フィールド名が違えば Task 9 Step 3 で合わせる
  - `NotifyEvent` variant は Task 6 Step 5 / Task 7 Step 4 で定義側を合わせる前提

---

## Execution Handoff

Plan complete. 次のいずれかで実行:

1. **Subagent-Driven (推奨)** — タスク毎に fresh subagent、二段レビュー (spec 準拠 → code quality)。早くて確実
2. **Inline Execution** — このセッション内で連続実行、checkpoint でユーザ確認

どちらで進めますか?
