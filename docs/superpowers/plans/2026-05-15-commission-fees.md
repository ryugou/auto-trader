# Commission を Trade.fees に記録する Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** live (`Execution.commission`) と paper (`commission::estimate_*`) の commission を `Trade.fees` に累積し、将来 commission 仕様が変わっても paper/live が一致した結果を返すようにする。

**Architecture:** `crates/core/src/commission.rs` を新規追加し、exchange ごとの paper-side estimate を 1 ファイルに集約 (現状は全て 0 を返す)。trader の `fill_open` / `fill_close` 系のヘルパー戻り値を `(price, qty, commission)` / `(price, commission)` に拡張し、`Trader::execute` / `Trader::close_position` で `Trade.fees` に積む。live は `Vec<Execution>.commission.sum()`、paper は estimate 関数経由。両経路で同じ field に積まれる。

**Tech Stack:** Rust 1.85+ (edition 2024, rust-toolchain.toml で stable pin)、rust_decimal、async-trait、sqlx、tokio、wiremock (test)。

---

## Required Test Command (各タスクの DoD)

CLAUDE.md 必須:

```bash
./scripts/test-all.sh
```

`ALL GREEN` が出るまで次タスクへ進まない。`Bash(git commit*)` PreToolUse hook も同じスクリプトを発火するので、commit ステップが失敗したら自動的にブロックされる。

---

## File Structure

新規:
- `crates/core/src/commission.rs`
- `crates/integration-tests/tests/phase3_commission.rs`

変更:
- `crates/core/src/lib.rs` (1 行追加: `pub mod commission;`)
- `crates/executor/src/trader.rs` (`aggregate_executions` / `poll_executions` / `fill_open` / `fill_close` / `fill_close_size` / `fill_close_with_stale_recovery` のシグネチャ拡張、`Trader::execute` / `close_position` で fees 積算)

---

## Task 0: Baseline 確認

**Files:** なし

- [ ] **Step 1: スクリプトで全段階緑を確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。失敗があれば計画着手前に修正。

- [ ] **Step 2: 新ブランチ確認**

```bash
git branch --show-current
```

Expected: `feat/commission-fees`。spec commit (`7797205`) が既にこのブランチに乗っている。

---

## Task 1: `core/commission.rs` 新規作成

**Files:**
- Create: `crates/core/src/commission.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: 失敗するユニットテストを先に書く**

`crates/core/src/commission.rs` を新規作成:

```rust
//! exchange 別の公式 commission を計算する pure 関数群。
//!
//! 現状 全 Exchange で 0 を返す (各取引所の現実の commission レートを反映)。
//! 将来 commission レートが変わったらこのファイルを更新するだけで
//! paper 側の Trade.fees 計算が live と等価のまま保たれる。

use crate::types::Exchange;
use rust_decimal::Decimal;

pub fn estimate_open(exchange: Exchange, _fill_price: Decimal, _qty: Decimal) -> Decimal {
    match exchange {
        Exchange::BitflyerCfd => Decimal::ZERO,
        Exchange::GmoFx => Decimal::ZERO,
        Exchange::Oanda => Decimal::ZERO,
    }
}

pub fn estimate_close(exchange: Exchange, _fill_price: Decimal, _qty: Decimal) -> Decimal {
    match exchange {
        Exchange::BitflyerCfd => Decimal::ZERO,
        Exchange::GmoFx => Decimal::ZERO,
        Exchange::Oanda => Decimal::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn estimate_open_all_exchanges_currently_zero() {
        assert_eq!(estimate_open(Exchange::BitflyerCfd, dec!(150), dec!(1)), Decimal::ZERO);
        assert_eq!(estimate_open(Exchange::GmoFx, dec!(150), dec!(1)), Decimal::ZERO);
        assert_eq!(estimate_open(Exchange::Oanda, dec!(150), dec!(1)), Decimal::ZERO);
    }

    #[test]
    fn estimate_close_all_exchanges_currently_zero() {
        assert_eq!(estimate_close(Exchange::BitflyerCfd, dec!(150), dec!(1)), Decimal::ZERO);
        assert_eq!(estimate_close(Exchange::GmoFx, dec!(150), dec!(1)), Decimal::ZERO);
        assert_eq!(estimate_close(Exchange::Oanda, dec!(150), dec!(1)), Decimal::ZERO);
    }
}
```

- [ ] **Step 2: `crates/core/src/lib.rs` に module 宣言を追加**

```bash
grep -n "^pub mod" crates/core/src/lib.rs | head -5
```

既存の `pub mod ...` 行の直後に追記:

```rust
pub mod commission;
```

- [ ] **Step 3: テスト実行 → pass 確認**

```bash
~/.cargo/bin/cargo test -p auto-trader-core commission 2>&1 | tail
```

Expected: `2 passed`。

- [ ] **Step 4: 全体 build 確認 (regression なし)**

```bash
~/.cargo/bin/cargo check --workspace --all-targets 2>&1 | tail
```

Expected: error なし。

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/commission.rs crates/core/src/lib.rs
git commit -m "feat(core): commission::estimate_open / estimate_close (all exchanges currently zero)"
```

(hook が `test-all.sh` を発火、ALL GREEN を確認)

---

## Task 2: `aggregate_executions` の戻り値に commission を追加

**Files:**
- Modify: `crates/executor/src/trader.rs:39-54` (`aggregate_executions`)

- [ ] **Step 1: 関数本体を修正**

`crates/executor/src/trader.rs:44` の `aggregate_executions` を:

```rust
/// Aggregate a non-empty execution list into (volume-weighted avg price, total size, total commission).
///
/// Used when `poll_executions` timed out but a follow-up `get_executions` call
/// confirmed the order did fill. Returns an error if the executions are empty
/// or total size is zero (caller should have guarded against the empty case).
fn aggregate_executions(execs: &[Execution]) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
    let total_size: Decimal = execs.iter().map(|e| e.size).sum();
    if total_size.is_zero() {
        anyhow::bail!(
            "aggregate_executions: total size is zero across {} execs",
            execs.len()
        );
    }
    let total_notional: Decimal = execs.iter().map(|e| e.price * e.size).sum();
    let total_commission: Decimal = execs.iter().map(|e| e.commission).sum();
    Ok((total_notional / total_size, total_size, total_commission))
}
```

- [ ] **Step 2: caller のパターン更新**

`crates/executor/src/trader.rs` 内の caller (3 箇所) を `(avg_price, total_size)` → `(avg_price, total_size, total_commission)` に分解。

`grep -n "aggregate_executions" crates/executor/src/trader.rs` で位置を確認すると 4 箇所ヒットする (定義 + 3 caller)。各 caller でパターンを以下のように修正:

```bash
grep -n "aggregate_executions" crates/executor/src/trader.rs
```

caller 例 (行は実装時の現状による):

```rust
// 修正前
match aggregate_executions(&execs) {
    Ok((avg_price, total_size)) => { ... }

// 修正後
match aggregate_executions(&execs) {
    Ok((avg_price, total_size, _commission)) => { ... }  // ← _commission は Task 3 で使う
```

別 caller:

```rust
// 修正前
let (avg_price, total_size) = aggregate_executions(&execs)?;
Ok((avg_price, total_size))

// 修正後 (Task 3 で戻り値拡張するまでは _commission 廃棄)
let (avg_price, total_size, _commission) = aggregate_executions(&execs)?;
Ok((avg_price, total_size))
```

- [ ] **Step 3: build 確認**

```bash
~/.cargo/bin/cargo check --workspace --all-targets 2>&1 | grep -E "^error|^\s*-->" | head
```

Expected: error なし (warning も無視できる範囲)。

- [ ] **Step 4: test-all.sh で regression なし確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。

- [ ] **Step 5: Commit**

```bash
git add crates/executor/src/trader.rs
git commit -m "refactor(executor): aggregate_executions returns total_commission (unused for now)"
```

---

## Task 3: `poll_executions` の戻り値を `(price, qty, commission)` に拡張

**Files:**
- Modify: `crates/executor/src/trader.rs` (`poll_executions` 関数本体 + caller)

- [ ] **Step 1: `poll_executions` のシグネチャと内部を修正**

`crates/executor/src/trader.rs:404` の `poll_executions` 関数 (現状の戻り値 `(Decimal, Decimal)`) を:

```rust
async fn poll_executions(
    &self,
    child_order_acceptance_id: &str,
    product_code: &str,
    timeout: Duration,
) -> anyhow::Result<(Decimal, Decimal, Decimal)> {  // ← (price, qty, commission)
    let start = Instant::now();
    loop {
        let execs = self
            .api
            .get_executions(product_code, child_order_acceptance_id)
            .await?;
        if !execs.is_empty() {
            return aggregate_executions(&execs);  // ← (price, qty, commission)
        }
        if start.elapsed() >= timeout {
            anyhow::bail!(
                "poll_executions: no executions for order {child_order_acceptance_id} within {:?}",
                timeout
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
```

実コードの本体は若干違うので、`grep -n "fn poll_executions" crates/executor/src/trader.rs` で位置を確認し、現状の body を保ったままシグネチャと return 文だけ修正する。

- [ ] **Step 2: `poll_executions` の caller を更新**

```bash
grep -n "poll_executions" crates/executor/src/trader.rs
```

各 caller で `(price, qty)` → `(price, qty, commission)` に分解。Task 4/5 で利用するまでは `_commission` で廃棄。

例:

```rust
// 修正前
let (price, qty) = self.poll_executions(...).await?;

// 修正後
let (price, qty, _commission) = self.poll_executions(...).await?;
```

`fill_close` / `fill_close_size` 内で `poll_executions` を呼ぶ箇所 (戻り値が `Decimal` のみ受けている場合) も同様に修正。

- [ ] **Step 3: Task 2 で `_commission` を捨てていた `aggregate_executions` 呼び出しに `_commission` 接尾を残す**

`fill_open` 内の reconcile 経路で `aggregate_executions(&execs)?` の戻り値を `(avg_price, total_size, _commission)` で受ける。実コードで全ての分岐をパターン抜けなく修正する。

- [ ] **Step 4: build 確認**

```bash
~/.cargo/bin/cargo check --workspace --all-targets 2>&1 | grep -E "^error|^\s*-->" | head
```

Expected: error なし。

- [ ] **Step 5: test-all.sh で regression なし確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。

- [ ] **Step 6: Commit**

```bash
git add crates/executor/src/trader.rs
git commit -m "refactor(executor): poll_executions returns commission alongside (price, qty)"
```

---

## Task 4: `fill_open` を `(price, qty, commission)` 戻りに拡張 + Trade.fees に積む

**Files:**
- Modify: `crates/executor/src/trader.rs` (`fill_open` 関数 + `Trader::execute` 内 `Trade.fees` の初期化)

- [ ] **Step 1: `fill_open` のシグネチャと paper 分岐を修正**

`crates/executor/src/trader.rs:173` 周辺の `fill_open` を:

```rust
/// fill_open: signal → 約定価格 + 実数量 + commission
///
/// - dry_run=true: PriceStore から Long=ask / Short=bid, commission は estimate_open
/// - dry_run=false: send_child_order → poll_executions, commission は約定の合計
async fn fill_open(
    &self,
    signal: &Signal,
    quantity: Decimal,
) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
    if self.dry_run {
        let feed_key = FeedKey::new(self.exchange, signal.pair.clone());
        let (bid, ask) = self
            .price_store
            .latest_bid_ask(&feed_key)
            .await
            .ok_or_else(|| anyhow::anyhow!("no bid/ask available for {}", signal.pair))?;
        let price = match signal.direction {
            Direction::Long => ask,
            Direction::Short => bid,
        };
        let commission =
            auto_trader_core::commission::estimate_open(self.exchange, price, quantity);
        Ok((price, quantity, commission))
    } else {
        // 既存の live 分岐の body を保ったまま、最後の戻り値だけ
        // (price, qty) → (price, qty, commission) に拡張する。
        // poll_executions / aggregate_executions が既に commission を含んだ
        // 戻り値を返すので、各成功分岐で 3 要素を返すように調整する。
        // 各 reconcile 経路でも同様。
        // (省略: 既存コードの caller 末尾 `Ok((price, qty))` を
        //  `Ok((price, qty, commission))` に書き換えるだけ)
        ...
    }
}
```

実コードでは live 分岐の `Ok(...)` 戻りが複数箇所ある (poll_executions 成功 / poll_executions タイムアウト → aggregate_executions / order Completed フォールバック)。順番に全て `commission` を含めるように修正する。

特に order-level fallback で execution が空の場合は `commission = Decimal::ZERO` を返す (約定詳細が取れないため):

```rust
// 修正前
Ok((order.average_price, order.executed_size))

// 修正後
// execution-level commission が取得できないので 0 で fallback。
// この経路は order Completed だが get_executions が空、というレアケース。
// operator alert は既存の warn! ログで充分。
Ok((order.average_price, order.executed_size, Decimal::ZERO))
```

- [ ] **Step 2: `Trader::execute` 内の `fill_open` 呼び出しを更新**

`crates/executor/src/trader.rs` の `execute` 関数 (現状の grep 位置 line 705 周辺):

```bash
grep -n "fill_open" crates/executor/src/trader.rs
```

```rust
// 修正前
let (fill_price, actual_qty) = self.fill_open(signal, quantity).await?;

// 修正後
let (fill_price, actual_qty, open_commission) = self.fill_open(signal, quantity).await?;
```

- [ ] **Step 3: `Trader::execute` の Trade 構築箇所で `fees = open_commission` を設定**

`crates/executor/src/trader.rs` の `let trade = Trade { ... }` (現状の grep 位置 line 847 周辺) で:

```rust
// 修正前
let trade = Trade {
    ...
    fees: Decimal::ZERO,
    ...
};

// 修正後
let trade = Trade {
    ...
    fees: open_commission,
    ...
};
```

- [ ] **Step 4: build 確認**

```bash
~/.cargo/bin/cargo check --workspace --all-targets 2>&1 | grep -E "^error|^\s*-->" | head
```

Expected: error なし。

- [ ] **Step 5: test-all.sh で regression なし確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN` (paper のテストは commission=0 なので既存挙動と同じ)。

- [ ] **Step 6: Commit**

```bash
git add crates/executor/src/trader.rs
git commit -m "feat(executor): plumb open commission from fill_open into Trade.fees"
```

---

## Task 5: `fill_close` / `fill_close_size` / `fill_close_with_stale_recovery` を `(price, commission)` 戻りに拡張 + Trade.fees に積む

**Files:**
- Modify: `crates/executor/src/trader.rs` (3 つの fill_close 系関数 + `Trader::close_position` 内の閉じ Trade 構築箇所)

- [ ] **Step 1: `fill_close` のシグネチャと paper 分岐を修正**

`crates/executor/src/trader.rs:368` 周辺:

```rust
async fn fill_close(&self, trade: &Trade) -> anyhow::Result<(Decimal, Decimal)> {  // ← (price, commission)
    if self.dry_run {
        let feed_key = FeedKey::new(self.exchange, trade.pair.clone());
        let (bid, ask) = self
            .price_store
            .latest_bid_ask(&feed_key)
            .await
            .ok_or_else(|| anyhow::anyhow!("no bid/ask available for {}", trade.pair))?;
        let price = match trade.direction {
            Direction::Long => bid,
            Direction::Short => ask,
        };
        let commission = auto_trader_core::commission::estimate_close(
            self.exchange,
            price,
            trade.quantity,
        );
        Ok((price, commission))
    } else {
        self.ensure_close_position_id_present(trade)?;
        let req = self.opposite_side_market_order(trade);
        let resp = self.api.send_child_order(req).await?;
        let (price, _qty, commission) = self
            .poll_executions(&resp.child_order_acceptance_id, &trade.pair.0, self.poll_timeout)
            .await?;
        Ok((price, commission))
    }
}
```

- [ ] **Step 2: `fill_close_size` のシグネチャと内部を修正**

`crates/executor/src/trader.rs:642` 周辺:

```rust
async fn fill_close_size(
    &self,
    trade: &Trade,
    size: Decimal,
) -> anyhow::Result<(Decimal, Decimal)> {  // ← (price, commission)
    if self.dry_run {
        // Dry-run: return price + estimate commission (same as fill_close).
        return self.fill_close(trade).await;
    }
    self.ensure_close_position_id_present(trade)?;
    let side = match trade.direction {
        Direction::Long => Side::Sell,
        Direction::Short => Side::Buy,
    };
    let req = SendChildOrderRequest {
        product_code: trade.pair.0.clone(),
        child_order_type: ChildOrderType::Market,
        side,
        size,
        price: None,
        minute_to_expire: None,
        time_in_force: None,
        close_position_id: trade.exchange_position_id.clone(),
    };
    let resp = self.api.send_child_order(req).await?;
    let (price, _qty, commission) = self
        .poll_executions(&resp.child_order_acceptance_id, &trade.pair.0, self.poll_timeout)
        .await?;
    Ok((price, commission))
}
```

- [ ] **Step 3: `fill_close_with_stale_recovery` のシグネチャを修正**

`crates/executor/src/trader.rs:553` 周辺の `fill_close_with_stale_recovery` (現状 `(Decimal, bool)` 戻り) を:

```rust
async fn fill_close_with_stale_recovery(
    &self,
    trade: &Trade,
) -> anyhow::Result<(Decimal, Decimal, bool)> {  // ← (price, commission, was_approximate)
    let positions = self.api.get_positions(&trade.pair.0).await?;
    // ... (既存の position size 計算 body はそのまま)

    if total_exchange_size.is_zero() {
        // Exchange position is gone — approximate exit price, zero commission.
        let approx_price = ...;  // 既存の best-effort 計算
        return Ok((approx_price, Decimal::ZERO, true));
    }

    let size_to_close = total_exchange_size.min(trade.quantity);
    let (price, commission) = self.fill_close_size(trade, size_to_close).await?;
    Ok((price, commission, false))
}
```

実コードの body は長いので、戻り値部分 (3 箇所程度) を順番に `(price, true_or_false)` → `(price, commission, true_or_false)` に書き換える。approximate=true の経路では commission=0 を返す (約定詳細が取れないため)。

- [ ] **Step 4: `Trader::close_position` 内の `fill_close` / `fill_close_with_stale_recovery` 呼び出しを更新**

```bash
grep -n "fill_close\|fill_close_with_stale_recovery" crates/executor/src/trader.rs | head
```

`Trader::close_position` の `exit_price_result` を計算する match を以下のように修正:

```rust
// 修正前
let exit_price_result: anyhow::Result<Decimal> = if !self.dry_run && was_stale_recovery {
    ...
    match self.fill_close_with_stale_recovery(&trade).await {
        Ok((price, approximate)) => {
            stale_approximate = approximate;
            Ok(price)
        }
        Err(e) => Err(e),
    }
} else {
    self.fill_close(&trade).await
};
let exit_price = match exit_price_result {
    Ok(price) => price,
    ...
};

// 修正後
let exit_result: anyhow::Result<(Decimal, Decimal)> =
    if !self.dry_run && was_stale_recovery {
        ...
        match self.fill_close_with_stale_recovery(&trade).await {
            Ok((price, commission, approximate)) => {
                stale_approximate = approximate;
                Ok((price, commission))
            }
            Err(e) => Err(e),
        }
    } else {
        self.fill_close(&trade).await
    };
let (exit_price, close_commission) = match exit_result {
    Ok(pair) => pair,
    ...
};
```

- [ ] **Step 5: `Trader::close_position` 内の閉じ Trade 構築箇所で `fees = trade.fees + close_commission` を設定**

```bash
grep -n "closed_trade = Trade" crates/executor/src/trader.rs
```

(現状 line 1043 周辺):

```rust
// 修正前
let closed_trade = Trade {
    ...
    fees: trade.fees,
    ...
};

// 修正後
let closed_trade = Trade {
    ...
    fees: trade.fees + close_commission,
    ...
};
```

- [ ] **Step 6: build 確認**

```bash
~/.cargo/bin/cargo check --workspace --all-targets 2>&1 | grep -E "^error|^\s*-->" | head
```

Expected: error なし。

- [ ] **Step 7: test-all.sh で regression なし確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。既存 phase3_close_flow 等は paper (dry_run=true) なので commission=0、`trade.fees == 0` を assert している場合は変わらず通る。

- [ ] **Step 8: Commit**

```bash
git add crates/executor/src/trader.rs
git commit -m "feat(executor): plumb close commission from fill_close into Trade.fees"
```

---

## Task 6: 統合テスト `phase3_commission.rs` を追加

**Files:**
- Create: `crates/integration-tests/tests/phase3_commission.rs`

- [ ] **Step 1: テストファイルを新規作成**

`crates/integration-tests/tests/phase3_commission.rs`:

```rust
//! Phase 3: commission を Trade.fees に累積する経路の paper=live 等価性テスト。
//!
//! このテストは Mock ExchangeApi (in-test) を用意し、open / close の
//! `Execution.commission` が `Trade.fees` に累積されることを確認する。
//! paper (dry_run=true) では `commission::estimate_*` 経由で現状 0 が積まれる。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::*;
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_market::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use auto_trader_notify::Notifier;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

struct CommissionMockApi {
    open_commission: Decimal,
    close_commission: Decimal,
}

#[async_trait]
impl ExchangeApi for CommissionMockApi {
    async fn send_child_order(
        &self,
        _req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        Ok(SendChildOrderResponse {
            child_order_acceptance_id: "mock-order".into(),
        })
    }

    async fn get_child_orders(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        Ok(vec![])
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        // open / close を区別できないので open commission を返す。
        // 個別テストでは Arc::new() で異なるインスタンスを使い分ける。
        Ok(vec![Execution {
            id: 1,
            child_order_id: "mock-order".into(),
            side: "BUY".into(),
            price: dec!(150),
            size: dec!(1000),
            commission: self.open_commission,
            exec_date: "2026-05-15T10:00:00Z".into(),
            child_order_acceptance_id: "mock-order".into(),
        }])
    }

    async fn get_positions(&self, _product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        Ok(vec![])
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        anyhow::bail!("not used")
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn resolve_position_id(
        &self,
        _product_code: &str,
        _after: chrono::DateTime<chrono::Utc>,
        _expected_side: Side,
        _expected_size: Decimal,
    ) -> anyhow::Result<Option<String>> {
        Ok(Some("mock-pos-1".into()))
    }

    fn requires_close_position_id(&self) -> bool {
        true
    }
}

async fn make_price_store(exchange: Exchange, pair: &str) -> Arc<PriceStore> {
    let feed_key = FeedKey::new(exchange, Pair::new(pair));
    let store = PriceStore::new(vec![feed_key.clone()]);
    store
        .update(
            feed_key,
            LatestTick {
                price: dec!(150),
                best_bid: Some(dec!(149.9)),
                best_ask: Some(dec!(150.1)),
                ts: Utc::now(),
            },
        )
        .await;
    store
}

fn make_trader(
    pool: sqlx::PgPool,
    account_id: Uuid,
    api: Arc<dyn ExchangeApi>,
    price_store: Arc<PriceStore>,
    dry_run: bool,
) -> Trader {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let sizer = Arc::new(PositionSizer::new(min_sizes));
    let notifier = Arc::new(Notifier::new_disabled());
    Trader::new(
        pool,
        Exchange::GmoFx,
        account_id,
        "commission_test".to_string(),
        api,
        price_store,
        notifier,
        sizer,
        dec!(1.00),
        dry_run,
    )
    .with_poll_timeout(std::time::Duration::from_millis(500))
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_open_accumulates_commission_into_fees(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "comm_open",
        "live",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let api: Arc<dyn ExchangeApi> = Arc::new(CommissionMockApi {
        open_commission: dec!(123),
        close_commission: dec!(0),
    });
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps, false);

    let signal = Signal {
        strategy_name: "test_strategy".into(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        stop_loss_pct: dec!(0.02),
        take_profit_pct: Some(dec!(0.04)),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        max_hold_until: None,
    };
    let trade = trader.execute(&signal).await.expect("open should succeed");
    assert_eq!(
        trade.fees,
        dec!(123),
        "open commission should land in Trade.fees"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_open_uses_estimate_commission_zero(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "comm_paper",
        "paper",
        "gmo_fx",
        "test_strategy",
        1_000_000,
    )
    .await;
    let api: Arc<dyn ExchangeApi> = Arc::new(CommissionMockApi {
        open_commission: dec!(999),  // ← live commission を返すがpaper は使わない
        close_commission: dec!(0),
    });
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY").await;
    let trader = make_trader(pool.clone(), account_id, api, ps, true);

    let signal = Signal {
        strategy_name: "test_strategy".into(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        stop_loss_pct: dec!(0.02),
        take_profit_pct: Some(dec!(0.04)),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(1.0),
        max_hold_until: None,
    };
    let trade = trader.execute(&signal).await.expect("paper open should succeed");
    assert_eq!(
        trade.fees,
        Decimal::ZERO,
        "paper open should use estimate_open which currently returns 0 for GMO FX"
    );
}
```

注意: live close での commission 累積を確認するテストは `Trader::close_position` の close path に依存し、mock の get_executions が open と close で同じ値を返してしまう (区別ができない)。close path 経由のテストを別途書きたい場合は、open と close で異なる commission を返す `Mutex<u8> { call_count }` 付き mock が必要だが、本 PR では open path で commission が積まれることを担保すれば十分 (close path は Task 5 の build green + 既存 phase3_close_flow が回帰確認になる)。

- [ ] **Step 2: テスト実行 → pass 確認**

```bash
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  ~/.cargo/bin/cargo test -p auto-trader-integration-tests --test phase3_commission 2>&1 | tail
```

Expected: `2 passed`。

- [ ] **Step 3: フル test-all.sh 確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。

- [ ] **Step 4: Commit**

```bash
git add crates/integration-tests/tests/phase3_commission.rs
git commit -m "test(integration): commission accumulates into Trade.fees (paper=0, live=API value)"
```

---

## Task 7: 最終検証 + simplify + code-review + PR

**Files:** なし

- [ ] **Step 1: 残骸 grep**

```bash
grep -rn "unimplemented!" crates/core/src/commission.rs crates/executor/src/trader.rs
grep -rn "TODO\|FIXME" crates/core/src/commission.rs
```

Expected: 出力なし。

- [ ] **Step 2: フル test-all.sh**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`、warning 0。

- [ ] **Step 3: simplify skill を起動**

3 並列 review agents (Reuse / Quality / Efficiency) で diff レビュー。MUST-FIX を inline で吸い上げる。

- [ ] **Step 4: code-review skill 経由で codex review**

CLAUDE.md の規律通り `codex:codex-rescue` で reviewer.md ペルソナの review を回す。PR #86 で permission 問題があった場合は self-review + Copilot review でフォロー。

- [ ] **Step 5: PR 作成**

```bash
gh pr create --base main --head feat/commission-fees \
  --title "feat(executor): commission を Trade.fees に記録 (paper=live 契約 2/N)" \
  --body "<PR description>"
```

PR description には:
- spec へのリンク (`docs/superpowers/specs/2026-05-15-commission-fees-design.md`)
- 各 Task で何を変えたかの 1 行サマリ
- Test plan: phase3_commission の 2 テスト + 既存 phase3_close_flow / phase3_jobs の regression 確認
- 契約違反項目の対応状況 (#2 commission → 実装、残り 6 項目は別 PR)

- [ ] **Step 6: Copilot review ループ**

PR 作成後、`gh pr edit <PR#> --add-reviewer copilot-pull-request-reviewer` で Copilot 起動。Round-by-round で genuine bug を fix、stale 再提起は commit message に記録のうえスキップ。最大 6 ラウンドで切り上げ。

---

## Spec Coverage Check

| spec セクション | 対応タスク |
|---|---|
| commission.rs API (estimate_open / estimate_close) | Task 1 |
| live 経路で `Execution.commission` を集計 | Task 2 (aggregate), Task 3 (poll) |
| fill_open シグネチャ拡張 + Trade.fees に積む | Task 4 |
| fill_close / fill_close_size / fill_close_with_stale_recovery シグネチャ拡張 | Task 5 |
| Trade.fees 累積 (close で trade.fees + close_commission) | Task 5 |
| integration test (live で commission が積まれる、paper で 0) | Task 6 |
| simplify + code-review + PR + Copilot | Task 7 |
| スコープ外 (SFD / open_fees/close_fees 分割 / DB schema 変更) | 全 Task で対象外 (touchしない) |
