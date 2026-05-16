# Paper liquidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** paper account のアカウント維持率が `liquidation_margin_level` を下回ったら、同 account の全 open trade を `ExitReason::Liquidation` で force-close する。

**Architecture:** `crates/core/src/margin.rs` を新規追加し、`OpenPosition` 構造体と `compute_maintenance_ratio` pure 関数 (IO 非依存) で計算ロジックを集約。`Trader::close_position` は既存経路を再利用。判定発火は `main.rs` の crypto position monitor の price tick ループ内、既存 SL/TP/TimeLimit 判定の直前。

**Tech Stack:** Rust 1.85+ (edition 2024、rust-toolchain.toml で stable pin)、rust_decimal、sqlx、tokio、tracing。

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
- `crates/core/src/margin.rs`
- `crates/integration-tests/tests/phase3_paper_liquidation.rs`

変更:
- `crates/core/src/lib.rs` (1 行追加: `pub mod margin;`)
- `crates/core/src/types.rs` (`ExitReason` enum + `as_str` + `FromStr` に `Liquidation` を追加)
- `crates/app/src/main.rs` (crypto monitor 内に維持率判定 + force-close ループを追加)

---

## Task 0: Baseline 確認

**Files:** なし

- [ ] **Step 1: スクリプトで全段階緑を確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。失敗があれば計画着手前に修正。

- [ ] **Step 2: ブランチ確認**

```bash
git branch --show-current
```

Expected: `feat/paper-liquidation`。spec commit は既にこのブランチに乗っている。

---

## Task 1: `ExitReason::Liquidation` variant 追加

**Files:**
- Modify: `crates/core/src/types.rs:143-200` (`ExitReason` enum + `as_str` + `FromStr`)

- [ ] **Step 1: テスト先行 — string round-trip + `is_liquidation` 識別**

`crates/core/src/types.rs` の既存テストモジュール末尾 (`#[cfg(test)] mod tests` があればそこへ。なければ新規 mod) に追加:

```rust
    #[test]
    fn exit_reason_liquidation_roundtrips_via_string() {
        let reason = ExitReason::Liquidation;
        assert_eq!(reason.as_str(), "liquidation");
        let parsed: ExitReason = "liquidation".parse().unwrap();
        assert_eq!(parsed.as_str(), "liquidation");
    }
```

- [ ] **Step 2: テスト fail 確認**

```bash
~/.cargo/bin/cargo test -p auto-trader-core exit_reason_liquidation_roundtrips 2>&1 | tail
```

Expected: コンパイルエラー (`Liquidation` variant 未定義)。

- [ ] **Step 3: enum + as_str + FromStr に `Liquidation` を追加**

`crates/core/src/types.rs:143` 周辺の `ExitReason` enum 末尾に variant 追加:

```rust
pub enum ExitReason {
    TpHit,
    SlHit,
    Manual,
    SignalReverse,
    StrategyMeanReached,
    StrategyTrailingChannel,
    StrategyTrailingMa,
    StrategyIndicatorReversal,
    StrategyTimeLimit,
    Reconciled,
    Liquidation,
}
```

`as_str` 実装 (167 行周辺) に arm 追加:

```rust
impl ExitReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            // ...既存 arm 群...
            ExitReason::Liquidation => "liquidation",
        }
    }
}
```

`FromStr` 実装 (185 行周辺) に arm 追加:

```rust
impl std::str::FromStr for ExitReason {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            // ...既存 arm 群...
            "liquidation" => Ok(ExitReason::Liquidation),
            other => anyhow::bail!("unknown ExitReason: {other}"),
        }
    }
}
```

`grep -n "ExitReason::" crates/core/src/types.rs` で正確な位置を確認してから差し込む。

- [ ] **Step 4: テスト pass 確認**

```bash
~/.cargo/bin/cargo test -p auto-trader-core exit_reason_liquidation_roundtrips 2>&1 | tail
```

Expected: PASS。

- [ ] **Step 5: 全体 build 確認**

```bash
~/.cargo/bin/cargo check --workspace --all-targets 2>&1 | grep -E "^error|^\s*-->" | head
```

Expected: error なし。`ExitReason` の match を扱う他の場所 (例えば format display) でも `Liquidation` arm 追加が必要なら別途対応。

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/types.rs
git commit -m "feat(core): ExitReason::Liquidation variant + string round-trip"
```

(hook が `test-all.sh` を発火、ALL GREEN を確認)

---

## Task 2: `core/margin.rs` 新規作成 (OpenPosition + compute_maintenance_ratio)

**Files:**
- Create: `crates/core/src/margin.rs`
- Modify: `crates/core/src/lib.rs` (1 行追加)

- [ ] **Step 1: ファイルを新規作成、unit test 5 件を先行**

`crates/core/src/margin.rs` を新規作成:

```rust
//! exchange-agnostic な維持率計算。pure 関数で IO 依存なし。
//!
//! 維持率 = (現金残高 + 評価損益合計) / 必要証拠金合計
//!
//! `Trader::close_position` の force-close 判定で使う。live exchange の
//! ロスカット式と同じ。

use crate::types::Direction;
use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenPosition {
    pub direction: Direction,
    pub entry_price: Decimal,
    pub current_price: Decimal,
    pub quantity: Decimal,
    pub leverage: Decimal,
}

impl OpenPosition {
    /// 評価損益 = (current - entry) * qty for Long, (entry - current) * qty for Short
    pub fn unrealized_pnl(&self) -> Decimal {
        let diff = match self.direction {
            Direction::Long => self.current_price - self.entry_price,
            Direction::Short => self.entry_price - self.current_price,
        };
        diff * self.quantity
    }

    /// 必要証拠金 = entry_price × quantity / leverage
    pub fn required_margin(&self) -> Decimal {
        self.entry_price * self.quantity / self.leverage
    }
}

/// 維持率 = (現金残高 + 評価損益合計) / 必要証拠金合計。
///
/// 必要証拠金合計が 0 (open position 無し) のとき `None` を返す。
/// 残高 + 評価損益が負になっても比率はそのまま負を返す (caller 側で
/// `< threshold` 比較に使える)。
pub fn compute_maintenance_ratio(
    current_balance: Decimal,
    positions: &[OpenPosition],
) -> Option<Decimal> {
    let total_required: Decimal = positions.iter().map(|p| p.required_margin()).sum();
    if total_required.is_zero() {
        return None;
    }
    let total_unrealized: Decimal = positions.iter().map(|p| p.unrealized_pnl()).sum();
    Some((current_balance + total_unrealized) / total_required)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn long_at(entry: Decimal, current: Decimal, qty: Decimal, lev: Decimal) -> OpenPosition {
        OpenPosition {
            direction: Direction::Long,
            entry_price: entry,
            current_price: current,
            quantity: qty,
            leverage: lev,
        }
    }

    fn short_at(entry: Decimal, current: Decimal, qty: Decimal, lev: Decimal) -> OpenPosition {
        OpenPosition {
            direction: Direction::Short,
            entry_price: entry,
            current_price: current,
            quantity: qty,
            leverage: lev,
        }
    }

    #[test]
    fn compute_maintenance_ratio_long_in_profit() {
        // balance=100k, entry=150, current=151, qty=10000, lev=25
        // required = 150*10000/25 = 60000
        // unrealized = (151-150)*10000 = 10000
        // ratio = (100000+10000)/60000 ≈ 1.8333
        let pos = long_at(dec!(150), dec!(151), dec!(10000), dec!(25));
        let ratio = compute_maintenance_ratio(dec!(100000), &[pos]).unwrap();
        assert_eq!(ratio, dec!(110000) / dec!(60000));
    }

    #[test]
    fn compute_maintenance_ratio_short_in_loss() {
        // balance=100k, entry=150, current=152, qty=10000, lev=25, Short
        // required = 60000
        // unrealized = (150-152)*10000 = -20000
        // ratio = (100000-20000)/60000 ≈ 1.333
        let pos = short_at(dec!(150), dec!(152), dec!(10000), dec!(25));
        let ratio = compute_maintenance_ratio(dec!(100000), &[pos]).unwrap();
        assert_eq!(ratio, dec!(80000) / dec!(60000));
    }

    #[test]
    fn compute_maintenance_ratio_multiple_positions_sum() {
        // 2 longs に分けて合計が単一ケースと一致することを確認
        let p1 = long_at(dec!(150), dec!(151), dec!(5000), dec!(25));
        let p2 = long_at(dec!(150), dec!(151), dec!(5000), dec!(25));
        let ratio_split = compute_maintenance_ratio(dec!(100000), &[p1, p2]).unwrap();
        let p_combined = long_at(dec!(150), dec!(151), dec!(10000), dec!(25));
        let ratio_single = compute_maintenance_ratio(dec!(100000), &[p_combined]).unwrap();
        assert_eq!(ratio_split, ratio_single);
    }

    #[test]
    fn compute_maintenance_ratio_zero_required_returns_none() {
        // 空 vec → required=0 → None
        assert!(compute_maintenance_ratio(dec!(100000), &[]).is_none());
    }

    #[test]
    fn compute_maintenance_ratio_negative_equity_returns_negative_ratio() {
        // balance=10000, big short loss → equity < 0、ratio も負
        let pos = short_at(dec!(150), dec!(170), dec!(10000), dec!(25));
        // required = 60000, unrealized = (150-170)*10000 = -200000
        // ratio = (10000-200000)/60000 ≈ -3.166
        let ratio = compute_maintenance_ratio(dec!(10000), &[pos]).unwrap();
        assert!(ratio.is_sign_negative());
    }
}
```

- [ ] **Step 2: `crates/core/src/lib.rs` に module 宣言を追加**

`grep -n "^pub mod" crates/core/src/lib.rs` で既存の `pub mod` 行を確認、`commission` の直後に挿入:

```rust
pub mod commission;
pub mod margin;
pub mod config;
// ...
```

- [ ] **Step 3: テスト pass 確認**

```bash
~/.cargo/bin/cargo test -p auto-trader-core margin 2>&1 | tail -10
```

Expected: `5 passed`。

- [ ] **Step 4: 全体 build 確認**

```bash
~/.cargo/bin/cargo check --workspace --all-targets 2>&1 | tail
```

Expected: error なし。

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/margin.rs crates/core/src/lib.rs
git commit -m "feat(core): margin::compute_maintenance_ratio + OpenPosition (pure, 5 unit tests)"
```

---

## Task 3: `main.rs` crypto monitor に維持率判定 + force-close 追加

**Files:**
- Modify: `crates/app/src/main.rs:836-1000` (crypto monitor の price tick ループ)

- [ ] **Step 1: 現状の crypto monitor ループ位置を確認**

```bash
grep -n "Task: Crypto position monitor\|exit_reason = match trade.direction" crates/app/src/main.rs | head
```

Expected: Crypto monitor の開始位置 (`Task: Crypto position monitor` コメント) と、既存 SL/TP 判定 (`let mut exit_reason = match trade.direction` 行) の位置。

- [ ] **Step 2: ロスカット判定 helper 関数を追加 (main.rs 内 or 別ファイル)**

main.rs はすでに大きいので、ロスカット判定 helper は別 module に出すのが望ましいが、今回は scope を膨らませず crypto monitor ループの直前にインライン関数で書く。loop の直前に以下を挿入:

```rust
use auto_trader_core::margin::{compute_maintenance_ratio, OpenPosition};

// closure として書く: open_trades 全体 + tick の event を見て、liquidation 発火対象の account_id 群を返す。
let detect_liquidation_accounts = |
    open_trades: &[auto_trader_db::trades::OpenTradeWithAccount],
    event: &PriceEvent,
    price_store: &Arc<PriceStore>,
    pool: &PgPool,
    exchange_liquidation_levels: &Arc<HashMap<Exchange, Decimal>>,
    live_forces_dry_run: bool,
| async move {
    // 実装は同期的に動かしたいが、price_store.latest_bid_ask は async なので
    // tokio runtime 上で sequential walk。詳細は Step 3 で示す。
};
```

注: closure では async 呼び出しが多重ネストになるので、別関数として置く方が読みやすい。次の Step で具体化する。

- [ ] **Step 3: helper 関数を別ファイルに置く**

scope 拡張を避けるため、helper は `crates/app/src/liquidation.rs` を新規作成し、そこに置く:

`crates/app/src/liquidation.rs` を新規作成:

```rust
//! paper account の維持率ロスカット判定。
//!
//! `Trader::close_position` を直接呼ばず、close 対象 trade_id の Vec を返すだけ
//! の純粋な判定 helper。main.rs の crypto monitor ループから呼び出される。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::margin::{compute_maintenance_ratio, OpenPosition};
use auto_trader_core::types::{Direction, Exchange};
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_market::price_store::{FeedKey, PriceStore};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

/// `event` の tick が来た時、同 exchange の paper account を walk して、
/// 維持率 `< threshold` の account の全 trade_id を返す。
///
/// 戻り値 `(account_id, vec_of_trade_ids)` の Vec。順次 close する想定。
pub async fn detect_liquidation_targets(
    open_trades: &[OpenTradeWithAccount],
    event: &PriceEvent,
    price_store: &Arc<PriceStore>,
    pool: &PgPool,
    exchange_liquidation_levels: &HashMap<Exchange, Decimal>,
    live_forces_dry_run: bool,
) -> Vec<(Uuid, Vec<Uuid>)> {
    // tick の exchange に該当する account_id を抽出 (重複排除)
    let tick_accounts: HashSet<Uuid> = open_trades
        .iter()
        .filter(|t| t.trade.exchange == event.exchange)
        .map(|t| t.trade.account_id)
        .collect();

    let threshold = match exchange_liquidation_levels.get(&event.exchange) {
        Some(t) => *t,
        None => return vec![], // 設定無しなら判定しない
    };

    let mut results = Vec::new();

    for account_id in tick_accounts {
        // 同 account の trades をフィルタ
        let trades_in_account: Vec<&OpenTradeWithAccount> = open_trades
            .iter()
            .filter(|t| t.trade.account_id == account_id)
            .collect();
        if trades_in_account.is_empty() {
            continue;
        }

        // account_type 判定。paper のみ対象。
        let account_type = trades_in_account
            .first()
            .and_then(|t| t.account_type.as_deref())
            .unwrap_or("paper");
        let dry_run = account_type == "paper" || live_forces_dry_run;
        if !dry_run {
            continue;
        }

        // account row を read
        let account = match auto_trader_db::trading_accounts::get_account(pool, account_id).await {
            Ok(Some(a)) => a,
            Ok(None) => {
                tracing::warn!(
                    "liquidation: account {account_id} not found (delete race?), skipping"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!("liquidation: failed to read account {account_id}: {e}");
                continue;
            }
        };

        // OpenPosition vec を組む。price 不在の trade があったら account 判定 skip
        // (false-positive Liquidation を避ける、保守的)。
        let mut positions = Vec::with_capacity(trades_in_account.len());
        let mut skip_account = false;
        for owned in &trades_in_account {
            let trade = &owned.trade;
            let feed_key = FeedKey::new(trade.exchange, trade.pair.clone());
            let bid_ask = price_store.latest_bid_ask(&feed_key).await;
            let current_price = match bid_ask {
                Some((bid, ask)) => match trade.direction {
                    // close-side bid/ask: Long close=bid, Short close=ask
                    Direction::Long => bid,
                    Direction::Short => ask,
                },
                None => {
                    tracing::warn!(
                        "liquidation: no price for {:?} {} — skipping account {account_id}",
                        trade.exchange,
                        trade.pair
                    );
                    skip_account = true;
                    break;
                }
            };
            positions.push(OpenPosition {
                direction: trade.direction,
                entry_price: trade.entry_price,
                current_price,
                quantity: trade.quantity,
                leverage: trade.leverage,
            });
        }
        if skip_account {
            continue;
        }

        // 維持率計算
        let ratio = match compute_maintenance_ratio(account.current_balance, &positions) {
            Some(r) => r,
            None => continue, // required=0、open 無し
        };

        if ratio < threshold {
            tracing::warn!(
                "liquidation: account {account_id} maintenance_ratio={ratio} < threshold={threshold} \
                 — force-closing {} trade(s)",
                trades_in_account.len()
            );
            let trade_ids: Vec<Uuid> = trades_in_account.iter().map(|t| t.trade.id).collect();
            results.push((account_id, trade_ids));
        }
    }

    results
}
```

`crates/app/src/lib.rs` (もしくは `main.rs` に mod 宣言) に `pub mod liquidation;` を追加。`grep -n "^pub mod\|^mod " crates/app/src/lib.rs` で既存 mod 一覧を確認、commission 後に挿入。

- [ ] **Step 4: crypto monitor ループ内で呼び出し追加**

`crates/app/src/main.rs` の crypto monitor の `for owned in open_trades` の **直前** (= `let open_trades = match ...` の取得後) に、ロスカット判定を追加。`grep -n "for owned in open_trades" crates/app/src/main.rs` で位置を確認、その直前に挿入:

```rust
// Liquidation 判定 (paper のみ): tick の event.exchange と一致する account を
// walk して、維持率が threshold を下回ったら同 account の全 trade を順次
// ExitReason::Liquidation で close する。SL/TP/TimeLimit 判定の前に走らせ、
// 既存 close 経路と排他は acquire_close_lock の natural CAS で成立する。
let liq_targets = auto_trader::liquidation::detect_liquidation_targets(
    &open_trades,
    &event,
    &crypto_monitor_price_store,
    &crypto_monitor_pool,
    &crypto_monitor_exchange_liquidation_levels,
    crypto_monitor_live_forces_dry_run,
)
.await;
for (account_id, trade_ids) in liq_targets {
    for trade_id in trade_ids {
        // close target trade を open_trades から探して Trader を組み立てる。
        let owned = match open_trades.iter().find(|t| t.trade.id == trade_id) {
            Some(o) => o,
            None => continue, // 既に他経路で close 済 (まれ)
        };
        let trade = &owned.trade;
        let account_name = owned
            .account_name
            .clone()
            .unwrap_or_else(|| account_id.to_string());
        let api: std::sync::Arc<dyn auto_trader_market::exchange_api::ExchangeApi> =
            match crypto_monitor_exchange_apis.get(&trade.exchange) {
                Some(a) => a.clone(),
                None => std::sync::Arc::new(
                    auto_trader_market::null_exchange_api::NullExchangeApi,
                ),
            };
        let liquidation_margin_level =
            match auto_trader::startup::liquidation_level_or_log(
                &crypto_monitor_exchange_liquidation_levels,
                trade.exchange,
                || format!("liquidation close trade {}", trade.id),
            ) {
                Some(y) => y,
                None => continue,
            };
        let trader = UnifiedTrader::new(
            crypto_monitor_pool.clone(),
            trade.exchange,
            account_id,
            account_name,
            api,
            crypto_monitor_price_store.clone(),
            crypto_monitor_notifier.clone(),
            crypto_monitor_position_sizer.clone(),
            liquidation_margin_level,
            true, // dry_run: paper のみここに来る
        );
        match trader
            .close_position(
                &trade.id.to_string(),
                auto_trader_core::types::ExitReason::Liquidation,
            )
            .await
        {
            Ok(_) => {
                tracing::info!(
                    "liquidation: trade {} closed (account {})",
                    trade.id,
                    account_id
                );
            }
            Err(e) => {
                tracing::warn!(
                    "liquidation: failed to close trade {} (account {}): {e} — continuing",
                    trade.id,
                    account_id
                );
            }
        }
    }
}
```

その後、既存の `for owned in open_trades` ループに進む。Liquidation で既に close した trade は SL/TP 判定の `acquire_close_lock` が `None` を返して natural skip する。

- [ ] **Step 5: build 確認**

```bash
~/.cargo/bin/cargo check --workspace --all-targets 2>&1 | grep -E "^error|^\s*-->" | head
```

Expected: error なし。`auto_trader::liquidation` モジュールが `lib.rs` 経由で publish されていれば main.rs から `auto_trader::liquidation::...` で呼べる。`grep -n "^pub mod\|^mod " crates/app/src/lib.rs` を見て module path を確認。

- [ ] **Step 6: フル test-all.sh で regression なし確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。Liquidation 用テストは Task 4 で追加するが、ここで既存 phase3 系が regression していないことを担保。

- [ ] **Step 7: Commit**

```bash
git add crates/app/src/liquidation.rs crates/app/src/lib.rs crates/app/src/main.rs
git commit -m "feat(app): paper liquidation detection + force-close in crypto monitor"
```

---

## Task 4: 統合テスト `phase3_paper_liquidation.rs` を追加

**Files:**
- Create: `crates/integration-tests/tests/phase3_paper_liquidation.rs`

- [ ] **Step 1: テストファイルを新規作成 (4 ケース)**

`crates/integration-tests/tests/phase3_paper_liquidation.rs`:

```rust
//! Phase 3: paper account の維持率ロスカット判定テスト。
//!
//! `detect_liquidation_targets` が paper account について、
//! 維持率 `< threshold` で正しく全 trade_id を返すこと、
//! および live account / price 不在の account を skip することを確認する。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Candle, Direction, Exchange, Pair, Trade, TradeStatus};
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use auto_trader_market::price_store::{FeedKey, LatestTick, PriceStore};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

async fn make_price_store(
    exchange: Exchange,
    pair: &str,
    bid: Decimal,
    ask: Decimal,
) -> Arc<PriceStore> {
    let feed_key = FeedKey::new(exchange, Pair::new(pair));
    let store = PriceStore::new(vec![feed_key.clone()]);
    store
        .update(
            feed_key,
            LatestTick {
                price: (bid + ask) / dec!(2),
                best_bid: Some(bid),
                best_ask: Some(ask),
                ts: Utc::now(),
            },
        )
        .await;
    store
}

fn make_event(exchange: Exchange, pair: &str, close: Decimal) -> PriceEvent {
    PriceEvent {
        pair: Pair::new(pair),
        exchange,
        timestamp: Utc::now(),
        candle: Candle {
            pair: Pair::new(pair),
            exchange,
            timeframe: "M5".to_string(),
            open: close,
            high: close,
            low: close,
            close,
            volume: dec!(0),
            timestamp: Utc::now(),
        },
        indicators: HashMap::new(),
    }
}

fn make_trade(
    account_id: Uuid,
    exchange: Exchange,
    pair: &str,
    direction: Direction,
    entry: Decimal,
    qty: Decimal,
    leverage: Decimal,
) -> Trade {
    Trade {
        id: Uuid::new_v4(),
        account_id,
        strategy_name: "test_strategy".into(),
        pair: Pair::new(pair),
        exchange,
        direction,
        entry_price: entry,
        exit_price: None,
        stop_loss: dec!(0),
        take_profit: None,
        quantity: qty,
        leverage,
        fees: dec!(0),
        entry_at: Utc::now(),
        exit_at: None,
        pnl_amount: None,
        exit_reason: None,
        status: TradeStatus::Open,
        max_hold_until: None,
        exchange_position_id: None,
    }
}

fn levels() -> HashMap<Exchange, Decimal> {
    let mut m = HashMap::new();
    m.insert(Exchange::GmoFx, dec!(1.00));
    m.insert(Exchange::BitflyerCfd, dec!(0.50));
    m
}

#[sqlx::test(migrations = "../../migrations")]
async fn liquidation_fires_when_maintenance_drops_below_threshold(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "liq_below",
        "paper",
        "gmo_fx",
        "test_strategy",
        100_000,
    )
    .await;
    let trade = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(10000),
        dec!(25),
    );
    auto_trader_db::trades::insert_trade(&pool, &trade)
        .await
        .unwrap();
    // margin lock to mirror open path
    let mut tx = pool.begin().await.unwrap();
    auto_trader_db::trades::lock_margin(
        &mut tx,
        account_id,
        trade.id,
        dec!(60000),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // current_price=148 → unrealized=(148-150)*10000=-20000、required=60000、
    // balance=100k-60k=40k (after margin lock)、equity=40k-20k=20k、ratio=20k/60k≈0.333 < 1.00
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY", dec!(148), dec!(148.1)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(148));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("liq_below".into()),
        account_type: Some("paper".into()),
    };
    let targets = auto_trader::liquidation::detect_liquidation_targets(
        &[owned],
        &event,
        &ps,
        &pool,
        &levels(),
        false,
    )
    .await;
    assert_eq!(targets.len(), 1, "one account should liquidate");
    assert_eq!(targets[0].0, account_id);
    assert_eq!(targets[0].1.len(), 1, "single trade in account");
}

#[sqlx::test(migrations = "../../migrations")]
async fn liquidation_does_not_fire_above_threshold(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "liq_above",
        "paper",
        "gmo_fx",
        "test_strategy",
        100_000,
    )
    .await;
    let trade = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(10000),
        dec!(25),
    );
    auto_trader_db::trades::insert_trade(&pool, &trade)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    auto_trader_db::trades::lock_margin(&mut tx, account_id, trade.id, dec!(60000))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // current=151 → unrealized=+10000、equity=40k+10k=50k、ratio=50k/60k=0.833、
    // ただし: balance after lock = 40000 なので equity=50000、ratio < 1.00 still。
    // 上回るには current を上げる: current=200 → unrealized=+500000、ratio>>1.00
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY", dec!(200), dec!(200.1)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(200));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("liq_above".into()),
        account_type: Some("paper".into()),
    };
    let targets = auto_trader::liquidation::detect_liquidation_targets(
        &[owned],
        &event,
        &ps,
        &pool,
        &levels(),
        false,
    )
    .await;
    assert!(targets.is_empty(), "no liquidation when ratio above threshold");
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_account_skips_liquidation_judgment(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "liq_live",
        "live",
        "gmo_fx",
        "test_strategy",
        100_000,
    )
    .await;
    let trade = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(10000),
        dec!(25),
    );
    auto_trader_db::trades::insert_trade(&pool, &trade)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    auto_trader_db::trades::lock_margin(&mut tx, account_id, trade.id, dec!(60000))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // current=148 → ratio < 1.00 だが live なので skip される
    let ps = make_price_store(Exchange::GmoFx, "USD_JPY", dec!(148), dec!(148.1)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(148));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("liq_live".into()),
        account_type: Some("live".into()),
    };
    let targets = auto_trader::liquidation::detect_liquidation_targets(
        &[owned],
        &event,
        &ps,
        &pool,
        &levels(),
        false, // live_forces_dry_run=false
    )
    .await;
    assert!(targets.is_empty(), "live account must not be liquidated by paper logic");
}

#[sqlx::test(migrations = "../../migrations")]
async fn missing_price_skips_judgment(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool,
        "liq_missing_price",
        "paper",
        "gmo_fx",
        "test_strategy",
        100_000,
    )
    .await;
    let trade = make_trade(
        account_id,
        Exchange::GmoFx,
        "USD_JPY",
        Direction::Long,
        dec!(150),
        dec!(10000),
        dec!(25),
    );
    auto_trader_db::trades::insert_trade(&pool, &trade)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    auto_trader_db::trades::lock_margin(&mut tx, account_id, trade.id, dec!(60000))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // PriceStore は EUR_USD だけ持つ → USD_JPY の price 不在
    let ps = make_price_store(Exchange::GmoFx, "EUR_USD", dec!(1.0), dec!(1.001)).await;
    let event = make_event(Exchange::GmoFx, "USD_JPY", dec!(148));

    let owned = OpenTradeWithAccount {
        trade,
        account_name: Some("liq_missing_price".into()),
        account_type: Some("paper".into()),
    };
    let targets = auto_trader::liquidation::detect_liquidation_targets(
        &[owned],
        &event,
        &ps,
        &pool,
        &levels(),
        false,
    )
    .await;
    assert!(targets.is_empty(), "missing price must skip judgment (false-positive prevention)");
}
```

- [ ] **Step 2: テスト実行**

```bash
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  ~/.cargo/bin/cargo test -p auto-trader-integration-tests --test phase3_paper_liquidation 2>&1 | tail -20
```

Expected: `4 passed`。

- [ ] **Step 3: フル test-all.sh 確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。

- [ ] **Step 4: Commit**

```bash
git add crates/integration-tests/tests/phase3_paper_liquidation.rs
git commit -m "test(integration): paper liquidation 4 cases (below/above threshold, live skip, missing price)"
```

---

## Task 5: simplify + 最終検証 + PR + Copilot

**Files:** なし

- [ ] **Step 1: 残骸 grep**

```bash
grep -rn "unimplemented!\|TODO\|FIXME" crates/core/src/margin.rs crates/app/src/liquidation.rs
```

Expected: 出力なし。

- [ ] **Step 2: フル test-all.sh**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`、warning 0。

- [ ] **Step 3: simplify skill を起動**

3 並列 review agents (Reuse / Quality / Efficiency) で diff レビュー。MUST-FIX を inline で吸い上げる。

- [ ] **Step 4: self-review + code-review**

CLAUDE.md 規律通り `code-review` skill 経由で codex review、permission 問題があれば reviewer.md self-review で代替 (ユーザーに確認)。

- [ ] **Step 5: PR 作成**

```bash
gh pr create --base main --head feat/paper-liquidation \
  --title "feat(app): paper liquidation (アカウント維持率ロスカット, paper=live 契約 4/N)" \
  --body "<PR description>"
```

PR description に含める:
- spec へのリンク (`docs/superpowers/specs/2026-05-16-paper-liquidation-design.md`)
- 各 Task のサマリ
- Test plan (margin unit tests 5 + integration tests 4 + regression 既存 phase3 系)
- 契約違反項目の対応状況 (#4 paper liquidation → 実装、残り SFD 等は別 PR)

- [ ] **Step 6: Copilot review ループ**

PR 作成後、`gh pr edit <PR#> --add-reviewer copilot-pull-request-reviewer` で Copilot 起動。Round-by-round で genuine bug を fix、stale 再提起は commit message に記録のうえスキップ。最大 6 ラウンドで切り上げ。

---

## Spec Coverage Check

| spec セクション | 対応タスク |
|---|---|
| `ExitReason::Liquidation` variant 追加 | Task 1 |
| `core/margin.rs` (OpenPosition + compute_maintenance_ratio) | Task 2 |
| main.rs 維持率判定 + force-close | Task 3 |
| `crates/app/src/liquidation.rs` (detect_liquidation_targets helper) | Task 3 |
| 全 4 ケース integration tests | Task 4 |
| 5 unit tests in margin.rs | Task 2 |
| Regression なし確認 | Task 3 Step 6, Task 4 Step 3 |
| simplify + code-review + PR + Copilot | Task 5 |
| Live account skip (account_type == "live" && !live_forces_dry_run) | Task 3 Step 3 (detect_liquidation_targets 内) + Task 4 (test) |
| Missing price skip (false-positive 防止) | Task 3 Step 3 + Task 4 (test) |
| 厳密下回り `<` で発火 | Task 3 Step 3 (`ratio < threshold`) |
| スコープ外 (live reconciler 統合 / swap / SFD / partial liquidation / 新 Slack カテゴリ) | 全 Task で対象外 |
