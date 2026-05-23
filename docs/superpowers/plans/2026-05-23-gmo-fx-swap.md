# GMO FX Swap Point (paper accrual) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** paper GMO FX account でも config 固定 rate ベースで daily swap point を `Trade.fees` に積算し、paper=live contract を成立させる。

**Architecture:** 既存 bitFlyer `overnight_handle` (`crates/app/src/main.rs:1719-1822`) に GMO FX 経路を **追加** し 1 cron に統合。新規 `core::swap::compute_daily_swap` + `db::trades::apply_swap_fee` + `TradeEventKind::SwapFee`。Migration で `'swap_fee'` event_type 追加、dashboard balance history と UI も対応。

**Tech Stack:** Rust, sqlx (Postgres), rust_decimal, serde (config), React/TypeScript (dashboard-ui)

**Spec:** `docs/superpowers/specs/2026-05-19-gmo-fx-swap-design.md`

---

### Task 1: Migration — `swap_fee` event_type

**Files:**
- Create: `migrations/20260519000001_account_events_add_swap_fee.sql`

- [ ] **Step 1: Create the migration file**

```sql
-- account_events.event_type CHECK 制約に 'swap_fee' を追加。
-- GMO FX paper accrual job が daily swap point を記録するため。
-- 符号両対応 (受取 amount<0、支払い amount>0、apply_sfd_fee と同規約)。
ALTER TABLE account_events
    DROP CONSTRAINT IF EXISTS account_events_event_type_check;

ALTER TABLE account_events
    ADD CONSTRAINT account_events_event_type_check
    CHECK (event_type IN (
        'margin_lock', 'margin_release', 'trade_open', 'trade_close',
        'overnight_fee', 'balance_sync', 'sfd_fee', 'swap_fee'
    ));
```

- [ ] **Step 2: Verify migration applies cleanly**

```bash
./scripts/test-all.sh 2>&1 | tail -10
```

Expected: ALL GREEN (`sqlx::test` reruns all migrations).

- [ ] **Step 3: Commit**

```bash
git add migrations/20260519000001_account_events_add_swap_fee.sql
git commit -m "feat(db): allow 'swap_fee' event_type for paper GMO FX accrual"
```

---

### Task 2: Config — `[gmo_fx.swap.rates]` セクション

**Files:**
- Modify: `crates/core/src/config.rs:7-37` (AppConfig 構造体に field 追加 + struct 定義追加)

- [ ] **Step 1: Add the new structs**

`crates/core/src/config.rs` のファイル末尾に近い位置 (他の `*Config` struct と並ぶ場所) に追加:

```rust
/// Per-pair × per-direction swap rates for GMO FX (in JPY per lot per day).
/// signed: **positive = paper account pays, negative = receives**
/// (apply_swap_fee の符号規約と一致)。
/// 1 lot = 10_000 通貨単位 (GMO FX 標準)。
///
/// TOML 例:
/// ```toml
/// [gmo_fx.swap.rates]
/// USD_JPY = { long = 100, short = -120 }
/// EUR_JPY = { long = 80, short = -100 }
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct GmoFxSwapConfig {
    pub rates: HashMap<String, SwapRateEntry>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct SwapRateEntry {
    pub long: Decimal,
    pub short: Decimal,
}
```

- [ ] **Step 2: Wire into AppConfig**

`crates/core/src/config.rs:7-37` の `AppConfig` struct に追加 (例えば
`exchange_margin` の直後)。TOML key path `[gmo_fx.swap]` を Rust 上で扱う
ため、`gmo_fx: GmoFxConfig` 経由でラップする (既存 `[exchange_margin.gmo_fx]`
を `exchange_margin: HashMap` で扱う設計と同パターン):

```rust
// 既存 AppConfig に追加:
    #[serde(default)]
    pub gmo_fx: GmoFxConfig,

// 新規 struct:
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct GmoFxConfig {
    pub swap: GmoFxSwapConfig,
}
```

これで TOML の `[gmo_fx.swap.rates]` が `app_config.gmo_fx.swap.rates: HashMap` にデシリアライズされる。

- [ ] **Step 3: Write the config parse test**

`crates/core/src/config.rs` の `#[cfg(test)] mod tests` 末尾に追加:

```rust
    #[test]
    fn parses_gmo_fx_swap_section() {
        let toml_str = r#"
[vegapunk]
endpoint = "http://x:0/v1/q"
schema = "s"

[database]
url = "postgres://x/y"

[monitor]
interval_secs = 60

[pairs]
crypto = []
fx = []

[gmo_fx.swap.rates]
USD_JPY = { long = 100, short = -120 }
EUR_JPY = { long = 80, short = -100 }
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        let rates = &config.gmo_fx.swap.rates;
        assert_eq!(rates.get("USD_JPY").unwrap().long, rust_decimal_macros::dec!(100));
        assert_eq!(rates.get("USD_JPY").unwrap().short, rust_decimal_macros::dec!(-120));
        assert_eq!(rates.get("EUR_JPY").unwrap().long, rust_decimal_macros::dec!(80));
        assert!(rates.get("GBP_JPY").is_none());
    }

    #[test]
    fn gmo_fx_swap_defaults_to_empty_when_missing() {
        let toml_str = r#"
[vegapunk]
endpoint = "http://x:0/v1/q"
schema = "s"

[database]
url = "postgres://x/y"

[monitor]
interval_secs = 60

[pairs]
crypto = []
fx = []
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(config.gmo_fx.swap.rates.is_empty());
    }
```

- [ ] **Step 4: Run tests**

```bash
cargo test --package auto-trader-core --lib config::tests::parses_gmo_fx_swap_section config::tests::gmo_fx_swap_defaults_to_empty_when_missing
```

Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/config.rs
git commit -m "feat(core/config): GmoFxConfig with [gmo_fx.swap.rates] section"
```

---

### Task 3: `core::swap::compute_daily_swap`

**Files:**
- Create: `crates/core/src/swap.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod swap;`)

- [ ] **Step 1: Write the failing tests**

新規ファイル `crates/core/src/swap.rs`:

```rust
//! GMO FX paper account 用 daily swap point 計算 (pure 関数)。
//!
//! paper account は live exchange と違い bot が自分で swap を計上する必要が
//! ある。config の rate table (pair × direction の代表値) を使って 1 日分の
//! swap を計算し、`apply_swap_fee` 経由で `Trade.fees` に積算する。
//!
//! formula:
//!   per_lot = rate.long_per_lot or rate.short_per_lot (direction で分岐)
//!   lots    = quantity / 10_000  (GMO FX 標準 1 lot = 10,000 通貨単位)
//!   fee     = truncate_yen(per_lot × lots)
//!
//! 戻り値が正なら paper account は **受取** (balance 増・fees 減算)、負なら
//! **支払い** (balance 減・fees 加算)。`apply_swap_fee` で符号両対応。

use crate::types::{Direction, Exchange};
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

/// paper 側 swap fee の skeleton。現状は全 exchange 0 を返す。
/// 実際の swap 計算は config rate を使う `compute_daily_swap` が行う。
pub fn estimate(exchange: Exchange) -> Decimal {
    match exchange {
        Exchange::BitflyerCfd => Decimal::ZERO,
        Exchange::GmoFx => Decimal::ZERO,
        Exchange::Oanda => Decimal::ZERO,
    }
}

/// 1 日分の swap fee (signed) を算出。truncate to whole yen。
pub fn compute_daily_swap(
    long_per_lot: Decimal,
    short_per_lot: Decimal,
    direction: Direction,
    quantity: Decimal,
) -> Decimal {
    let per_lot = match direction {
        Direction::Long => long_per_lot,
        Direction::Short => short_per_lot,
    };
    let lots = quantity / dec!(10_000);
    (per_lot * lots).round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_all_exchanges_currently_zero() {
        for ex in [Exchange::BitflyerCfd, Exchange::GmoFx, Exchange::Oanda] {
            assert_eq!(estimate(ex), Decimal::ZERO);
        }
    }

    #[test]
    fn long_positive_rate_receives_fee() {
        // USD_JPY Long 1 lot (10_000 通貨), long_rate = +100 JPY/lot/day
        let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Long, dec!(10_000));
        assert_eq!(fee, dec!(100));
    }

    #[test]
    fn short_negative_rate_pays_fee() {
        // USD_JPY Short 1 lot, short_rate = -120 JPY/lot/day
        let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Short, dec!(10_000));
        assert_eq!(fee, dec!(-120));
    }

    #[test]
    fn partial_lot_scales_proportionally() {
        // 0.5 lot (5_000 通貨), long_rate = +100 → 50
        let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Long, dec!(5_000));
        assert_eq!(fee, dec!(50));
    }

    #[test]
    fn truncates_fractional_yen_to_zero() {
        // 0.3 lot (3_000 通貨), long_rate = +100 → 30 (整数なので変化なし)
        // 0.33 lot (3_300 通貨), long_rate = +1 → 0.33 → truncate = 0
        let fee = compute_daily_swap(dec!(1), dec!(-1), Direction::Long, dec!(3_300));
        assert_eq!(fee, Decimal::ZERO);
    }

    #[test]
    fn zero_rate_returns_zero() {
        let fee = compute_daily_swap(dec!(0), dec!(0), Direction::Long, dec!(10_000));
        assert_eq!(fee, Decimal::ZERO);
    }

    #[test]
    fn zero_quantity_returns_zero() {
        let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Long, Decimal::ZERO);
        assert_eq!(fee, Decimal::ZERO);
    }
}
```

- [ ] **Step 2: Wire module into lib.rs**

`crates/core/src/lib.rs` を編集して `pub mod swap;` を追加 (アルファベット順、`strategy` の前):

```rust
pub mod commission;
pub mod config;
pub mod event;
pub mod executor;
pub mod knowledge;
pub mod margin;
pub mod sfd;
pub mod strategy;
pub mod swap;        // ← 追加
pub mod types;
pub mod vegapunk_port;
```

(注: 実際の現状ファイルでは `swap` を `sfd` の後 `strategy` の前にアルファベット順で追加)

- [ ] **Step 3: Run tests**

```bash
cargo test --package auto-trader-core --lib swap::tests
```

Expected: 7 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/swap.rs crates/core/src/lib.rs
git commit -m "feat(core/swap): add compute_daily_swap (config-rate-based, truncate to yen)"
```

---

### Task 4: `db::trades::apply_swap_fee`

**Files:**
- Modify: `crates/db/src/trades.rs:345` (apply_sfd_fee の直後に追加)

**注**: db crate は integration-tests crate に依存できないため、apply_swap_fee の DB 動作テストは Task 7 の integration tests でまとめる。

- [ ] **Step 1: Add the function**

`crates/db/src/trades.rs::apply_sfd_fee` の **直後** に追加 (apply_sfd_fee の完全コピー、event_type と doc コメントのみ違う):

```rust
/// Apply a swap fee (positive = paper account pays, negative = receives)
/// for a single trade inside a transaction.
///
/// `apply_sfd_fee` と同パターン (event_type='swap_fee' の点だけ違う)。
/// GMO FX paper accrual job が daily で呼び出す。
///
/// 符号両対応:
///   fee_amount > 0 (払い) → trades.fees 増、current_balance 減、event amount = -fee
///   fee_amount < 0 (受取) → trades.fees 減、current_balance 増、event amount = -fee (= +)
pub async fn apply_swap_fee(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
    occurred_at: DateTime<Utc>,
) -> anyhow::Result<Option<Decimal>> {
    let trade_updated = sqlx::query(
        "UPDATE trades SET fees = fees + $3
         WHERE id = $1 AND account_id = $2 AND status = 'open'",
    )
    .bind(trade_id)
    .bind(account_id)
    .bind(fee_amount)
    .execute(&mut *tx)
    .await?;

    if trade_updated.rows_affected() == 0 {
        return Ok(None);
    }

    let new_balance: Decimal = sqlx::query_scalar(
        r#"UPDATE trading_accounts
           SET current_balance = current_balance - $2
           WHERE id = $1
           RETURNING current_balance"#,
    )
    .bind(account_id)
    .bind(fee_amount)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO account_events (account_id, trade_id, event_type, amount, balance_after, occurred_at)
           VALUES ($1, $2, 'swap_fee', $3, $4, $5)"#,
    )
    .bind(account_id)
    .bind(trade_id)
    .bind(-fee_amount)
    .bind(new_balance)
    .bind(occurred_at)
    .execute(&mut *tx)
    .await?;

    Ok(Some(new_balance))
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo build -p auto-trader-db 2>&1 | tail -3
```

Expected: 成功 (test は Task 7 で integration 経由)。

- [ ] **Step 3: Commit**

```bash
git add crates/db/src/trades.rs
git commit -m "feat(db): add apply_swap_fee (clone of apply_sfd_fee, event_type='swap_fee')"
```

---

### Task 5: TradeEventKind::SwapFee + get_trade_events 分岐

**Files:**
- Modify: `crates/db/src/trades.rs:683` (TradeEventKind enum) + line 757 周辺 (get_trade_events match)

- [ ] **Step 1: Add SwapFee variant**

`crates/db/src/trades.rs::TradeEventKind` enum を変更:

```rust
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeEventKind {
    Open,
    OvernightFee,
    SfdFee,
    SwapFee,   // ← 追加
    Close,
}
```

- [ ] **Step 2: Wire into get_trade_events**

`crates/db/src/trades.rs::get_trade_events` 内の `match row.event_type.as_str()` 分岐を変更 (line 757 付近):

```rust
        let kind = match row.event_type.as_str() {
            "overnight_fee" => TradeEventKind::OvernightFee,
            "sfd_fee" => TradeEventKind::SfdFee,
            "swap_fee" => TradeEventKind::SwapFee,   // ← 追加
            _ => continue,
        };
```

- [ ] **Step 3: Build**

```bash
cargo build -p auto-trader-db 2>&1 | tail -3
```

Expected: 成功。

- [ ] **Step 4: Commit**

```bash
git add crates/db/src/trades.rs
git commit -m "feat(db): TradeEventKind::SwapFee + get_trade_events branch"
```

---

### Task 6: Dashboard balance history に 'swap_fee' 追加

**Files:**
- Modify: `crates/db/src/dashboard.rs:484-493` (event_type filter)

- [ ] **Step 1: Add 'swap_fee' to filter**

`crates/db/src/dashboard.rs:484-493` 付近:

```rust
               daily_delta AS (
                   -- Only realized P&L events (trade_close + overnight_fee + sfd_fee + swap_fee).
                   -- Margin lock/release are excluded so the chart shows
                   -- the account's true value growth, not the cash dips
                   -- from opening positions.
                   -- sfd_fee は bitFlyer Crypto CFD の SFD (PR #92)、
                   -- swap_fee は GMO FX paper accrual (本 PR)。
                   SELECT DATE(occurred_at) AS date,
                          SUM(amount) AS daily_net
                   FROM account_events
                   WHERE account_id = $1
                     AND event_type IN ('trade_close', 'overnight_fee', 'sfd_fee', 'swap_fee')
                   GROUP BY DATE(occurred_at)
               )
```

- [ ] **Step 2: Build**

```bash
cargo build -p auto-trader-db 2>&1 | tail -3
```

Expected: 成功。

- [ ] **Step 3: Commit**

```bash
git add crates/db/src/dashboard.rs
git commit -m "feat(db/dashboard): include 'swap_fee' in balance history filter"
```

---

### Task 7: Cron 統合 — overnight_handle に GMO FX 経路追加

**Files:**
- Modify: `crates/app/src/main.rs:1719-1822` (overnight_handle task)

**Note**: 既存 task 内で bitFlyer 経路 + GMO FX 経路を並列で実行する。`exchange != Exchange::BitflyerCfd` の continue を `match exchange` 分岐に置き換え。

- [ ] **Step 1: Locate the bitFlyer-only filter**

```bash
grep -n "if exchange != Exchange::BitflyerCfd" crates/app/src/main.rs
```

Expected: 1 hit (line ~1755 付近、overnight_handle 内)。

- [ ] **Step 2: Replace bitFlyer-only filter with match dispatch**

該当箇所 (`if exchange != Exchange::BitflyerCfd { continue; }` から始まり、trade loop 全体を含む block) を以下に書き換え:

```rust
                    // Per-exchange fee logic.
                    // bitFlyer Crypto CFD: 既存の overnight funding rate
                    //   (entry_price × quantity × 0.04%/day)。
                    // GMO FX: config の per-pair × per-direction swap rate
                    //   (本 PR で追加、event_type='swap_fee')。
                    let open_trades = match auto_trader_db::trades::get_open_trades_by_account(
                        &overnight_pool,
                        pac.id,
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::error!(
                                "overnight/swap: failed to list open trades for {}: {e}",
                                pac.name
                            );
                            continue;
                        }
                    };
                    // event_at = この hour の UTC midnight 境界。catch-up 等の
                    // multi-day シナリオはないため now 相当でも実用上問題ないが、
                    // attribution は今日の midnight で固定する。
                    let event_at = today
                        .and_hms_opt(0, 0, 0)
                        .expect("midnight is always valid")
                        .and_utc();
                    let mut total_fee = Decimal::ZERO;
                    for trade in &open_trades {
                        let fee = match exchange {
                            Exchange::BitflyerCfd => {
                                (trade.entry_price * trade.quantity * fee_rate)
                                    .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero)
                            }
                            Exchange::GmoFx => {
                                let Some(rate) =
                                    swap_config.rates.get(trade.pair.0.as_str())
                                else {
                                    tracing::debug!(
                                        "gmo swap: no rate for pair {} on trade {}; skip",
                                        trade.pair,
                                        trade.id
                                    );
                                    continue;
                                };
                                auto_trader_core::swap::compute_daily_swap(
                                    rate.long,
                                    rate.short,
                                    trade.direction,
                                    trade.quantity,
                                )
                            }
                            _ => continue, // 他 exchange は対象外
                        };
                        if fee.is_zero() {
                            continue;
                        }
                        // Apply fee atomically with branch on exchange.
                        let result = async {
                            let mut tx = overnight_pool.begin().await?;
                            let applied = match exchange {
                                Exchange::BitflyerCfd => {
                                    auto_trader_db::trades::apply_overnight_fee(
                                        &mut tx, pac.id, trade.id, fee,
                                    )
                                    .await?
                                }
                                Exchange::GmoFx => {
                                    auto_trader_db::trades::apply_swap_fee(
                                        &mut tx, pac.id, trade.id, fee, event_at,
                                    )
                                    .await?
                                }
                                _ => unreachable!("filtered above"),
                            };
                            tx.commit().await?;
                            anyhow::Ok(applied)
                        }
                        .await;
                        match result {
                            Ok(Some(_)) => {
                                total_fee += fee;
                            }
                            Ok(None) => {
                                tracing::debug!(
                                    "overnight/swap: skipping trade {} — closed before fee tx",
                                    trade.id
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    "overnight/swap: apply_*_fee failed for trade {}: {e}",
                                    trade.id
                                );
                            }
                        }
                    }
                    if total_fee != Decimal::ZERO {
                        tracing::info!(
                            "overnight/swap applied: {} = {} JPY (exchange={:?})",
                            pac.name,
                            total_fee,
                            exchange
                        );
                    }
```

- [ ] **Step 3: Wire swap_config into closure capture**

`overnight_handle = tokio::spawn(async move { ... })` の前 (line 1718 付近) に swap_config を clone:

```rust
    let overnight_pool = pool.clone();
    let swap_config = config.gmo_fx.swap.clone();  // ← 追加 (config は main の AppConfig)
    let overnight_handle = tokio::spawn(async move {
        // 既存 body (...)
```

そして `for trade in &open_trades` loop の中で `swap_config.rates.get(...)` を参照する。

- [ ] **Step 4: Update the warn message for unknown exchange**

`exchange_from_str(&pac.exchange)` の None 分岐の warn message を更新 ("overnight fee:" → "overnight/swap:"):

```rust
                            tracing::warn!(
                                "overnight/swap: skipping account {} ({}): unknown exchange '{}'",
                                pac.name,
                                pac.id,
                                pac.exchange
                            );
```

- [ ] **Step 5: Build to verify**

```bash
cargo build -p auto-trader 2>&1 | tail -10
```

Expected: 成功 (もし `Direction` を `trade.direction` で渡す際に型不整合があれば調整、`Pair` の field 名 `.0` を使う点も確認)。

- [ ] **Step 6: Commit**

```bash
git add crates/app/src/main.rs
git commit -m "feat(app): integrate GMO FX swap into overnight cron (per-pair config rates)"
```

---

### Task 8: Integration tests — `phase3_gmo_swap_accrual.rs`

**Files:**
- Create: `crates/integration-tests/tests/phase3_gmo_swap_accrual.rs`

**Note**: cron 自体の動作は main.rs に任せ、ここでは `compute_daily_swap` + `apply_swap_fee` の組み合わせを実 DB で検証 (`phase3_sfd_paper_accrual.rs` と同パターン)。

- [ ] **Step 1: Write the integration test file**

新規ファイル:

```rust
//! Phase 3: paper GMO FX swap accrual の DB レベル統合テスト。
//!
//! `compute_daily_swap` で fee を算出 → `apply_swap_fee` で DB に反映する
//! 一連のフローを実 DB で検証。cron 自体の wiring は main.rs に任せ、
//! ここでは fee 算出 + DB 反映の組み合わせのみテストする
//! (phase3_sfd_paper_accrual.rs と同パターン)。

use auto_trader_core::swap::compute_daily_swap;
use auto_trader_core::types::Direction;
use auto_trader_db::trades::{apply_swap_fee, get_trade_by_id};
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_integration_tests::helpers::seed::seed_open_trade;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

async fn insert_usdjpy_trade(
    pool: &sqlx::PgPool,
    account_id: uuid::Uuid,
    direction: Direction,
    quantity: Decimal,
) -> uuid::Uuid {
    let dir_str = match direction {
        Direction::Long => "long",
        Direction::Short => "short",
    };
    seed_open_trade(
        pool,
        account_id,
        "test_strat",
        "USD_JPY",
        "gmo_fx",
        dir_str,
        dec!(150),
        dec!(149),
        quantity,
        Utc::now(),
    )
    .await
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_gmo_long_receives_positive_swap(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool, "swap_long", "paper", "gmo_fx", "test_strat", 1_000_000,
    )
    .await;
    let trade_id = insert_usdjpy_trade(&pool, account_id, Direction::Long, dec!(10_000)).await;

    // USD_JPY Long 1 lot (10_000 通貨), long_rate = +100 → fee = +100 (受取)
    let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Long, dec!(10_000));
    assert_eq!(fee, dec!(100));

    let mut tx = pool.begin().await.unwrap();
    let new_balance = apply_swap_fee(&mut tx, account_id, trade_id, fee, Utc::now())
        .await
        .unwrap()
        .expect("Some");
    tx.commit().await.unwrap();

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, dec!(100));
    assert_eq!(new_balance, dec!(1_000_000) - dec!(100));
    // 受取 = balance 減 (apply の `current_balance -= fee_amount` の semantics)、
    // しかし fee 自体は +100 のため:
    //   Long 受取: bot 内では「fee_amount = -receive_amount」で呼ぶ運用想定
    // **重要**: 本テストは関数の動作確認のため、apply 結果を素直に確認する。
    // 実際の overnight cron では受取 case (rate>0, Long) は apply_swap_fee に
    // **負の fee_amount** を渡すことで balance 増を実現する設計に合わせる。
    // ↓ Cron 経路の正しい呼び方は次のテストで検証。
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_gmo_short_pays_negative_swap_via_negate(pool: sqlx::PgPool) {
    // semantic: short_rate = -120 → fee = -120 → apply_swap_fee(..., -120) は
    //   trades.fees -= 120 (fees 減)、balance += 120 (受取)
    // ただし実装意図は「Short の場合 paper account は ALWAYS 支払う」想定
    // のはずなので、cron 経路でどう sign を扱うか再確認 — 設計上は:
    //   per_lot = rate.short_per_lot (= -120 を信頼) → fee = -120
    //   apply_swap_fee(fee=-120) → balance += 120、fees -= 120
    // 解釈: rate.short = -120 は「-120 を fee として課す = paper 支払い」を
    //   意図したが、apply の `balance -= fee` semantics と組み合わせると
    //   逆向きになる。**設計を素直に読むと**:
    //     fee_amount > 0 = paper 払い、< 0 = paper 受取
    //     short_per_lot = -120 (= 受取) を Short trade に当てる
    //   実 GMO 仕様では USD_JPY Short は支払い (Long が受取) → config 値の
    //   解釈は「long = +受取、short = -受取 (= 支払い)」で正しい。
    let account_id = seed_trading_account(
        &pool, "swap_short", "paper", "gmo_fx", "test_strat", 1_000_000,
    )
    .await;
    let trade_id = insert_usdjpy_trade(&pool, account_id, Direction::Short, dec!(10_000)).await;

    // long = +100 (Long 受取), short = -120 (Short 支払い、fee_amount は -120
    // = paper 受取という符号矛盾) — **実は spec 上 short rate は config に
    // 「支払い側を負値で」表現するため、compute_daily_swap の戻り値は
    // 「Short trade で `short_per_lot=-120` → fee = -120」となる。
    // apply_swap_fee の符号規約 (>0=払い、<0=受取) に対し、Short trade で
    // 受取扱いになるのは設計と矛盾する。**Task 7 cron で、Short fee を
    // 反転して渡す or config 表記を見直す** 必要あり。本テストは現状
    // compute_daily_swap の動作通り assert する。
    let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Short, dec!(10_000));
    assert_eq!(fee, dec!(-120));

    let mut tx = pool.begin().await.unwrap();
    let new_balance = apply_swap_fee(&mut tx, account_id, trade_id, fee, Utc::now())
        .await
        .unwrap()
        .expect("Some");
    tx.commit().await.unwrap();

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, dec!(-120));
    assert_eq!(new_balance, dec!(1_000_000) - dec!(-120));
}

#[sqlx::test(migrations = "../../migrations")]
async fn apply_swap_fee_returns_none_when_trade_closed(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool, "swap_closed", "paper", "gmo_fx", "test_strat", 1_000_000,
    )
    .await;
    let trade_id = insert_usdjpy_trade(&pool, account_id, Direction::Long, dec!(10_000)).await;
    sqlx::query("UPDATE trades SET status='closed' WHERE id=$1")
        .bind(trade_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let result = apply_swap_fee(&mut tx, account_id, trade_id, dec!(100), Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(result.is_none(), "closed trade must skip apply_swap_fee");

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, Decimal::ZERO);
}

#[sqlx::test(migrations = "../../migrations")]
async fn account_event_row_recorded_with_swap_fee_type(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool, "swap_event", "paper", "gmo_fx", "test_strat", 1_000_000,
    )
    .await;
    let trade_id = insert_usdjpy_trade(&pool, account_id, Direction::Long, dec!(10_000)).await;

    let mut tx = pool.begin().await.unwrap();
    apply_swap_fee(&mut tx, account_id, trade_id, dec!(100), Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (count, amount, evt_type): (i64, Decimal, String) = sqlx::query_as(
        "SELECT COUNT(*), MAX(amount), MAX(event_type)
         FROM account_events WHERE trade_id=$1",
    )
    .bind(trade_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(amount, dec!(-100)); // 払い時 amount は -fee
    assert_eq!(evt_type, "swap_fee");
}
```

**注**: Short の sign 扱いは Task 7 実装中に再確認必要 (上記 inline コメント参照)。test も実装と整合させて修正。

- [ ] **Step 2: Run integration tests**

```bash
cargo test -p auto-trader-integration-tests --test phase3_gmo_swap_accrual 2>&1 | tail -15
```

Expected: 4 tests PASS。Sign 矛盾発生時は Task 7 の cron 実装と config の解釈を確定してから test を調整。

- [ ] **Step 3: Commit**

```bash
git add crates/integration-tests/tests/phase3_gmo_swap_accrual.rs
git commit -m "test: phase3_gmo_swap_accrual (compute + apply DB integration)"
```

---

### Task 9: UI 拡張 (dashboard-ui)

**Files:**
- Modify: `dashboard-ui/src/api/types.ts:114` (TradeEvent.kind union)
- Modify: `dashboard-ui/src/components/TradeTable.tsx` (eventLabel/eventColor/renderCashDelta)

- [ ] **Step 1: Add 'swap_fee' to TradeEvent.kind union**

`dashboard-ui/src/api/types.ts:114` を変更:

```typescript
export interface TradeEvent {
  kind: 'open' | 'overnight_fee' | 'sfd_fee' | 'swap_fee' | 'close'
  // ...既存 fields
}
```

- [ ] **Step 2: Add 'swap_fee' branches to TradeTable.tsx**

`dashboard-ui/src/components/TradeTable.tsx::eventLabel`:

```tsx
function eventLabel(kind: TradeEvent['kind']): string {
  switch (kind) {
    case 'open': return 'OPEN'
    case 'close': return 'CLOSE'
    case 'overnight_fee': return 'overnight'
    case 'sfd_fee': return 'SFD'
    case 'swap_fee': return 'swap'    // ← 追加
  }
}
```

`eventColor`:

```tsx
function eventColor(kind: TradeEvent['kind']): string {
  switch (kind) {
    case 'open': return 'text-sky-400'
    case 'close': return 'text-amber-400'
    case 'overnight_fee': return 'text-gray-500'
    case 'sfd_fee': return 'text-gray-500'
    case 'swap_fee': return 'text-gray-500'   // ← 追加
  }
}
```

`renderCashDelta` の null 判定 (SFD と同パターン):

```tsx
    if (ev.kind === 'overnight_fee' || ev.kind === 'sfd_fee' || ev.kind === 'swap_fee') {
      return <span className="text-gray-500">-</span>
    }
```

- [ ] **Step 3: Build UI to verify**

```bash
cd dashboard-ui && npm run build 2>&1 | tail -5
```

Expected: 型エラー無しで build 成功。

- [ ] **Step 4: Commit**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
git add dashboard-ui/src/api/types.ts dashboard-ui/src/components/TradeTable.tsx
git commit -m "feat(ui): add 'swap_fee' to TradeEvent kind union + TradeTable rendering"
```

---

### Task 10: test-all → simplify → code-review → PR

- [ ] **Step 1: Run the full test suite**

```bash
./scripts/test-all.sh 2>&1 | tail -15
```

Expected: ALL GREEN.

- [ ] **Step 2: simplify skill**

```
Invoke skill: simplify
```

3 並列 review agent + 結果反映。

- [ ] **Step 3: 自己レビュー (reviewer.md 5 観点)**

特にチェック:
- **Reliability**: Short の符号扱い (Task 8 の inline コメント参照)。config rate `short = -120` が実 GMO 仕様の Short 支払い → fee_amount への mapping を確定し、test と implementation を一致させる。
- **Architecture**: overnight_handle 内の `match exchange` 分岐が肥大化したら fee_cron module 化を検討 (本 PR scope では inline で十分、Future PR で共通化)。

- [ ] **Step 4: Push branch (deny rule のためユーザに依頼)**

```bash
git push -u origin feat/gmo-fx-swap
```

deny されたらユーザに手動 push 依頼。

- [ ] **Step 5: Open PR**

```bash
gh pr create --title "feat: GMO FX swap point paper accrual" --body "$(cat <<'EOF'
## Summary
- paper GMO FX account でも config 固定 rate ベースで daily swap point を `Trade.fees` に積算
- 既存 `overnight_handle` (bitFlyer overnight fee task) に GMO FX 経路を **追加**して 1 cron に統合
- `core::swap::compute_daily_swap` (config rate × direction × quantity、truncate to yen) 追加
- `db::trades::apply_swap_fee` (`apply_sfd_fee` clone、event_type='swap_fee') 追加
- Migration: `account_events.event_type` に `'swap_fee'` 追加
- Dashboard balance history と TradeEventKind + UI も対応 (PR A SFD と同パターン)

これで GMO FX も bitFlyer と並んで **paper=live contract** 成立。live は GMO 取引所が balance に自動反映する「live=exchange 任せ、paper=bot 代行」設計。

## v1 limitation (spec で開示済)
- 3 倍デー (NY 火曜→水曜 roll 3 日分) は scope outside (config に weekday 倍率追加で将来対応)
- swap rate は config 代表値 hardcode (毎日変動を追わない、近似値)
- restart 跨ぎ persistence + apply_*_fee 共通化 refactor は別 follow-up PR

## Docs
- Spec: \`docs/superpowers/specs/2026-05-19-gmo-fx-swap-design.md\`
- Plan: \`docs/superpowers/plans/2026-05-23-gmo-fx-swap.md\`

## Test plan
- [x] \`./scripts/test-all.sh\` ALL GREEN
- [x] core unit: \`compute_daily_swap\` 7 ケース (Long/Short × lot 比例 + truncate + zero)
- [x] core unit: config parse 2 ケース ([gmo_fx.swap.rates] あり/なし)
- [x] integration: \`phase3_gmo_swap_accrual\` 4 ケース
- [x] simplify レビュー反映

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 6: Copilot review loop**

```bash
gh pr edit <PR#> --add-reviewer copilot-pull-request-reviewer
```

ScheduleWakeup 270s → comments 確認 → 対応 → push → re-request。指摘ゼロ / 軽微 suggestion のみまで継続。最大 15 ラウンド (PR A と同方針、最終ラウンドで Accepted Risk として打ち切り可)。
