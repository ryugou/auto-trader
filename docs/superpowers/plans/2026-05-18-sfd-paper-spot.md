# bitFlyer SFD Paper Accrual Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** bitFlyer Crypto CFD の SFD を paper account でも実計算し、`Trade.fees` に hourly accrual する。これで paper=live contract のロジック差を消す (残るはレイテンシ起因 slippage のみ)。

**Architecture:** 既存 bitFlyer WS の subscribe pair に `BTC_JPY` (現物) を追加 (tick forwarding を builder mount より前に移動して PriceStore に流す) → 新規 hourly cron job が paper bitflyer open trade 全件を sweep し、`compute_hourly_sfd` で fee 算出 → `apply_sfd_fee` で atomic に `trades.fees` 加算 + balance 差引 + `account_events` 記録。live は PR #91 の `fetch_close_sfd` のまま (double counting なし)。

**Tech Stack:** Rust, sqlx (Postgres), tokio, rust_decimal

**Spec:** `docs/superpowers/specs/2026-05-18-sfd-paper-spot-design.md`

---

### Task 1: Migration — account_events.event_type に 'sfd_fee' 追加

**Files:**
- Create: `migrations/20260518000001_account_events_add_sfd_fee.sql`

- [ ] **Step 1: Create the migration file**

```sql
-- account_events.event_type CHECK 制約に 'sfd_fee' を追加。
-- SFD (bitFlyer Crypto CFD の現物-FX 乖離手数料) accrual job が
-- paper account に対して fee 行を記録するため。受け取り SFD では
-- amount が正、支払 SFD では負 (overnight_fee と同じ符号規約)。
ALTER TABLE account_events
    DROP CONSTRAINT account_events_event_type_check;

ALTER TABLE account_events
    ADD CONSTRAINT account_events_event_type_check
    CHECK (event_type IN (
        'margin_lock', 'margin_release', 'trade_open', 'trade_close',
        'overnight_fee', 'balance_sync', 'sfd_fee'
    ));
```

- [ ] **Step 2: Verify migration applies cleanly**

```bash
./scripts/test-all.sh 2>&1 | tail -10
```

Expected: ALL GREEN (`sqlx::test` reruns all migrations against ephemeral DBs).

- [ ] **Step 3: Commit**

```bash
git add migrations/20260518000001_account_events_add_sfd_fee.sql
git commit -m "feat(db): allow 'sfd_fee' event_type for paper SFD accrual"
```

---

### Task 2: `core::sfd::sfd_daily_rate` 階段関数 + tests

**Files:**
- Modify: `crates/core/src/sfd.rs` (add new function at end of file, before `#[cfg(test)]`)

- [ ] **Step 1: Write the failing tests**

`crates/core/src/sfd.rs` の `mod tests` の末尾に追加:

```rust
    #[test]
    fn sfd_daily_rate_below_5pct_is_zero() {
        assert_eq!(sfd_daily_rate(dec!(0)), Decimal::ZERO);
        assert_eq!(sfd_daily_rate(dec!(0.04999)), Decimal::ZERO);
    }

    #[test]
    fn sfd_daily_rate_5_to_10pct_is_0_25pct() {
        assert_eq!(sfd_daily_rate(dec!(0.05)), dec!(0.0025));
        assert_eq!(sfd_daily_rate(dec!(0.09999)), dec!(0.0025));
    }

    #[test]
    fn sfd_daily_rate_10_to_15pct_is_0_5pct() {
        assert_eq!(sfd_daily_rate(dec!(0.10)), dec!(0.005));
        assert_eq!(sfd_daily_rate(dec!(0.14999)), dec!(0.005));
    }

    #[test]
    fn sfd_daily_rate_15_to_20pct_is_1pct() {
        assert_eq!(sfd_daily_rate(dec!(0.15)), dec!(0.01));
        assert_eq!(sfd_daily_rate(dec!(0.19999)), dec!(0.01));
    }

    #[test]
    fn sfd_daily_rate_20pct_or_more_is_3pct() {
        assert_eq!(sfd_daily_rate(dec!(0.20)), dec!(0.03));
        assert_eq!(sfd_daily_rate(dec!(0.50)), dec!(0.03));
    }

    #[test]
    fn sfd_daily_rate_normalizes_negative_input_via_abs() {
        // sfd_daily_rate は内部で .abs() を取るため、負値も対応する正の
        // region と同じ rate を返す (Copilot review round-3 で footgun
        // 解消、pub function として安全に使える)。
        assert_eq!(sfd_daily_rate(dec!(-0.10)), dec!(0.005));
        assert_eq!(sfd_daily_rate(dec!(-0.04)), Decimal::ZERO);
    }
```

- [ ] **Step 2: Run tests, verify they fail to compile**

```bash
cargo test --package auto-trader-core --lib sfd::tests::sfd_daily_rate
```

Expected: FAIL with `cannot find function 'sfd_daily_rate'`.

- [ ] **Step 3: Add the function**

`crates/core/src/sfd.rs` の `estimate` 関数の **直後** (`#[cfg(test)]` の前) に追加:

```rust
/// bitFlyer Crypto CFD 公式 SFD 階段 (**daily** rate)。
///
/// 入力 `divergence` は乖離率 (例: `dec!(0.07)` = 7%、負値も可)。
/// 内部で `.abs()` を取るため呼び出し側は符号を気にせず渡せる
/// (Copilot review round-3 で pub function の footgun を内部正規化で解消)。
///
///   |x| < 5%        → 0.00%
///   5%  ≤ |x| < 10% → 0.25%
///   10% ≤ |x| < 15% → 0.50%
///   15% ≤ |x| < 20% → 1.00%
///   20% ≤ |x|       → 3.00%
///
/// bitFlyer Crypto CFD 公式 docs に基づく。rate 改定時は本関数の階段値を更新。
pub fn sfd_daily_rate(divergence: Decimal) -> Decimal {
    use rust_decimal_macros::dec;
    let d = divergence.abs();
    if d < dec!(0.05) {
        Decimal::ZERO
    } else if d < dec!(0.10) {
        dec!(0.0025)
    } else if d < dec!(0.15) {
        dec!(0.005)
    } else if d < dec!(0.20) {
        dec!(0.01)
    } else {
        dec!(0.03)
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test --package auto-trader-core --lib sfd::tests::sfd_daily_rate
```

Expected: 6 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/sfd.rs
git commit -m "feat(core/sfd): add sfd_daily_rate step function (bitFlyer official)"
```

---

### Task 3: `core::sfd::compute_hourly_sfd` + `SfdContext` + tests

**Files:**
- Modify: `crates/core/src/sfd.rs`

- [ ] **Step 1: Write the failing tests**

`crates/core/src/sfd.rs` の `mod tests` 末尾に追加:

```rust
    fn ctx(fx: Decimal, spot: Decimal, notional: Decimal, dir: Direction) -> SfdContext {
        SfdContext {
            fx_price: fx,
            spot_price: spot,
            position_notional: notional,
            direction: dir,
        }
    }

    #[test]
    fn compute_hourly_sfd_zero_when_divergence_below_5pct() {
        // 4% 乖離 → SFD = 0
        let c = ctx(dec!(104), dec!(100), dec!(100_000), Direction::Long);
        assert_eq!(compute_hourly_sfd(c), Decimal::ZERO);
    }

    #[test]
    fn compute_hourly_sfd_long_pays_when_fx_above_spot() {
        // FX=110, spot=100 → 乖離 +10% → rate 0.5% (daily), hourly = 0.5%/24
        // notional=100_000 → hourly fee = 100_000 * 0.005 / 24 = 20.833...
        // Long で FX>spot → 払う (+)
        let c = ctx(dec!(110), dec!(100), dec!(100_000), Direction::Long);
        let fee = compute_hourly_sfd(c);
        assert!(fee > Decimal::ZERO, "Long pays when FX>spot, got {fee}");
        let expected = dec!(100_000) * dec!(0.005) / dec!(24);
        assert_eq!(fee, expected);
    }

    #[test]
    fn compute_hourly_sfd_short_receives_when_fx_above_spot() {
        // 同じ乖離だが Short → 受け取る (-)
        let c = ctx(dec!(110), dec!(100), dec!(100_000), Direction::Short);
        let fee = compute_hourly_sfd(c);
        assert!(fee < Decimal::ZERO, "Short receives when FX>spot, got {fee}");
        let expected = -(dec!(100_000) * dec!(0.005) / dec!(24));
        assert_eq!(fee, expected);
    }

    #[test]
    fn compute_hourly_sfd_long_receives_when_fx_below_spot() {
        // FX=90, spot=100 → 乖離 -10% → rate 0.5% (abs), Long で FX<spot → 受け取る (-)
        let c = ctx(dec!(90), dec!(100), dec!(100_000), Direction::Long);
        let fee = compute_hourly_sfd(c);
        assert!(fee < Decimal::ZERO);
        let expected = -(dec!(100_000) * dec!(0.005) / dec!(24));
        assert_eq!(fee, expected);
    }

    #[test]
    fn compute_hourly_sfd_short_pays_when_fx_below_spot() {
        let c = ctx(dec!(90), dec!(100), dec!(100_000), Direction::Short);
        let fee = compute_hourly_sfd(c);
        assert!(fee > Decimal::ZERO);
        let expected = dec!(100_000) * dec!(0.005) / dec!(24);
        assert_eq!(fee, expected);
    }

    #[test]
    fn compute_hourly_sfd_zero_spot_returns_zero_no_panic() {
        // spot=0 だと divergence 計算で除算エラーになる。安全弁として 0 を返すべき。
        let c = ctx(dec!(100), Decimal::ZERO, dec!(100_000), Direction::Long);
        assert_eq!(compute_hourly_sfd(c), Decimal::ZERO);
    }
```

- [ ] **Step 2: Run tests, verify they fail to compile**

```bash
cargo test --package auto-trader-core --lib sfd::tests::compute_hourly_sfd
```

Expected: FAIL with `cannot find type 'SfdContext'`.

- [ ] **Step 3: Add the struct + function**

`crates/core/src/sfd.rs` の冒頭 `use` 群の直後に `Direction` import を追加:

```rust
use crate::types::{Direction, Exchange};
use rust_decimal::Decimal;
```

(現状 `use crate::types::Exchange;` のみなので置換)

`sfd_daily_rate` の **直後** に追加:

```rust
/// SFD 計算に必要な context。
///
/// `position_notional` は `entry_price × quantity` の絶対値 (建玉評価額)。
/// `fx_price` / `spot_price` はいずれも JPY 建ての価格。
#[derive(Debug, Clone, Copy)]
pub struct SfdContext {
    pub fx_price: Decimal,
    pub spot_price: Decimal,
    pub position_notional: Decimal,
    pub direction: Direction,
}

/// 1 時間分の SFD を算出する。
///
/// formula:
///   divergence = (fx - spot) / spot
///   daily_rate = sfd_daily_rate(|divergence|)
///   hourly_fee = notional × daily_rate / 24
///   sign:
///     fx > spot かつ Long  →  +fee (払う)
///     fx > spot かつ Short → -fee (受け取る)
///     fx < spot かつ Long  → -fee (受け取る)
///     fx < spot かつ Short → +fee (払う)
///
/// `spot_price` が 0 / 負の場合は安全弁として `Decimal::ZERO` を返す
/// (除算エラー回避)。
pub fn compute_hourly_sfd(ctx: SfdContext) -> Decimal {
    if ctx.spot_price <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let divergence = (ctx.fx_price - ctx.spot_price) / ctx.spot_price;
    let rate = sfd_daily_rate(divergence.abs());
    if rate.is_zero() {
        return Decimal::ZERO;
    }
    let magnitude = ctx.position_notional * rate / Decimal::from(24);
    let sign_positive = match (divergence.is_sign_positive(), ctx.direction) {
        (true, Direction::Long) => true,    // FX>spot, Long → 払う
        (true, Direction::Short) => false,  // FX>spot, Short → 受け取る
        (false, Direction::Long) => false,  // FX<spot, Long → 受け取る
        (false, Direction::Short) => true,  // FX<spot, Short → 払う
    };
    if sign_positive { magnitude } else { -magnitude }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test --package auto-trader-core --lib sfd::tests::compute_hourly_sfd
```

Expected: 6 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/sfd.rs
git commit -m "feat(core/sfd): add SfdContext + compute_hourly_sfd"
```

---

### Task 4: bitFlyer WS — tick forwarding を builder mount より前に移動

**Files:**
- Modify: `crates/market/src/bitflyer.rs:364-400` (ticker handling 内)

**背景:** 現状の WS handler は `let Some(builder) = builders.get_mut(product_code) else { continue; };` で builder mount されていない pair の tick を捨てる。BTC_JPY (現物) は SFD 計算のために PriceStore に届ける必要があるが、candle/strategy には不要なため builder を mount したくない。tick forwarding を builder mount より前に移動して両立させる。

- [ ] **Step 1: Read the current ticker handler to confirm boundaries**

```bash
grep -n "let Some(builder) = builders.get_mut" crates/market/src/bitflyer.rs
```

Expected: 1 行ヒット (line 368 付近)。

- [ ] **Step 2: Refactor the handler**

該当箇所を以下に変更 (該当の前後 30 行ほどを下記に揃える):

```rust
        let Some(params) = rpc.params else { continue };
        let ticker = params.message;
        let product_code = &ticker.product_code;

        let price = ticker.ltp;
        let size = ticker.volume;
        let best_bid = Some(ticker.best_bid);
        let best_ask = Some(ticker.best_ask);
        let ts =
            chrono::DateTime::parse_from_rfc3339(&ticker.timestamp)?.with_timezone(&chrono::Utc);

        // PriceStore への raw tick forwarding は builder mount の有無に
        // 関係なく行う。BTC_JPY (現物 spot) のように strategy/candle は
        // 不要だが SFD 計算等で PriceStore.latest_bid_ask が要る pair
        // があるため。drain task の channel が満杯なら drop (次の tick で
        // 埋まる、許容)。
        if let Err(e) = tick_tx.try_send((
            FeedKey::new(Exchange::BitflyerCfd, Pair::new(product_code)),
            LatestTick {
                price,
                best_bid,
                best_ask,
                ts,
            },
        )) {
            tracing::trace!("bitflyer tick_tx full, dropping {}: {e}", product_code);
        }

        let Some(builder) = builders.get_mut(product_code) else {
            // no candle/strategy mounted for this product — tick already
            // forwarded to PriceStore above, nothing else to do.
            continue;
        };
```

- [ ] **Step 3: Delete the old in-builder tick_tx.try_send block**

builder block 内 (line ~384 付近) の旧 `tick_tx.try_send((...))` 呼び出しと付随する `FeedKey::new(...)` / `LatestTick { ... }` を **削除** (上記 Step 2 で前に移動済みなので二重送信になる)。

確認手順:

```bash
grep -c "tick_tx.try_send" crates/market/src/bitflyer.rs
```

Expected: `1` (1 箇所のみに集約されたこと)。

- [ ] **Step 4: Add BTC_JPY to subscribe pair when WS pair list contains FX_BTC_JPY**

`crates/market/src/bitflyer.rs` 内、`feed_loop_inner` を呼ぶ前段で subscribe pair リストを構築している箇所を確認するため:

```bash
grep -n "feed_loop_inner\|FX_BTC_JPY\|spawn\|new(ws_url" crates/market/src/bitflyer.rs | head -20
```

`BitflyerFeed::new(ws_url, pairs, ...)` の `pairs` に FX_BTC_JPY が含まれている場合に **BTC_JPY を自動追加** する処理を `new()` 内に追加:

```rust
    pub fn new(ws_url: &str, pairs: Vec<Pair>, timeframe: &str) -> Self {
        // SFD 計算用に BTC_JPY (現物) を自動追加。
        // FX_BTC_JPY を subscribe するなら必ず spot も subscribe して
        // PriceStore に流し、paper SFD accrual job (app::main) が
        // 乖離率を計算できるようにする。strategy/candle は mount しない
        // ので strategy 経路には影響しない。
        let mut pairs = pairs;
        let has_fx_btc = pairs.iter().any(|p| p.0 == "FX_BTC_JPY");
        let has_spot_btc = pairs.iter().any(|p| p.0 == "BTC_JPY");
        if has_fx_btc && !has_spot_btc {
            pairs.push(Pair::new("BTC_JPY"));
        }
        Self {
            ws_url: ws_url.to_string(),
            pairs,
            timeframe: timeframe.to_string(),
            pool: None,
            closes_seed: HashMap::new(),
            candle_seeds: HashMap::new(),
        }
    }
```

(現状の `Self { ... }` リテラルに合わせて挿入。フィールドが違ったら現実に合わせる。)

- [ ] **Step 5: Run market crate tests to verify no regression**

```bash
cargo test --package auto-trader-market --lib bitflyer
```

Expected: existing tests PASS (line 502-578 付近の FX_BTC_JPY pair fixture を使うテストが影響を受けないこと)。

- [ ] **Step 6: Commit**

```bash
git add crates/market/src/bitflyer.rs
git commit -m "feat(market/bitflyer): forward BTC_JPY spot tick to PriceStore

WS handler の tick_tx.try_send を builder mount より前に移動。BTC_JPY
(現物 spot) は strategy 不要だが SFD 計算で PriceStore.latest_bid_ask
が要るため、builder mount せずに PriceStore にだけ流す。new() で
FX_BTC_JPY が含まれる場合 BTC_JPY を自動 subscribe。"
```

---

### Task 5: `db::trades::apply_sfd_fee` (実装のみ、test は Task 7)

**Files:**
- Modify: `crates/db/src/trades.rs`

**注**: `db` crate は `integration-tests` crate に依存できない (循環)。`apply_sfd_fee` の DB 動作テストは Task 7 の integration-tests crate にまとめる (既存 `apply_overnight_fee` も同じく `phase3_integrity.rs` の integration test で検証している pattern を踏襲)。

このタスクでは関数追加のみ。

- [ ] **Step 1: Add the function**

`crates/db/src/trades.rs::apply_overnight_fee` の **直後** に追加 (apply_overnight_fee を base にコピーし、event_type と符号処理を変更):

```rust
/// Apply an SFD fee (positive = paper account pays, negative = receives)
/// for a single trade inside a transaction.
///
/// Atomically:
///   1. CAS on `status='open'` & account_id match (Ok(None) if no row)
///   2. `trades.fees += fee_amount` (negative amount decreases fees)
///   3. `trading_accounts.current_balance -= fee_amount`
///      (negative amount → balance increases = received SFD)
///   4. Insert `account_events` row with `event_type='sfd_fee'`,
///      `amount = -fee_amount` (outflow when fee positive, inflow when negative)
///
/// Returns `Ok(Some(new_balance))` when applied, `Ok(None)` when the trade
/// was no longer open.
pub async fn apply_sfd_fee(
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
           VALUES ($1, $2, 'sfd_fee', $3, $4, $5)"#,
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
git commit -m "feat(db): add apply_sfd_fee (positive=pay, negative=receive)"
```

---

### Task 6: Hourly SFD accrual job in `app/main.rs`

**Files:**
- Modify: `crates/app/src/main.rs` (新規 task を `overnight_handle` の隣に追加)

- [ ] **Step 1: Locate insertion point**

```bash
grep -n "overnight_handle\|Task: Overnight fee" crates/app/src/main.rs
```

`Task: Overnight fee` コメントブロックの直後 (~line 1822、`overnight_handle` の `tokio::spawn` 終了の直後) に新規 task を挿入する。

- [ ] **Step 2: Add the SFD hourly task**

`overnight_handle = tokio::spawn(...)` の閉じカッコ直後に追加:

```rust
    // Task: SFD (bitFlyer Crypto CFD) hourly accrual for paper accounts.
    // bitFlyer's official SFD charges open positions at each hour boundary
    // based on the spot/FX divergence. Live trades read API-actual SFD via
    // fetch_close_sfd at close time (PR #91). Paper trades need an in-bot
    // hourly job to mirror the same accrual; this is that job.
    let sfd_pool = pool.clone();
    let sfd_price_store = price_store.clone();
    let sfd_handle = tokio::spawn(async move {
        use auto_trader_core::sfd;
        use auto_trader_market::price_store::FeedKey;
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        let mut last_hour: Option<chrono::DateTime<chrono::Utc>> = None;
        loop {
            interval.tick().await;
            let now = chrono::Utc::now();
            // hour boundary detection: trigger when the hour rolls over.
            let current_hour = now.date_naive().and_hms_opt(now.hour(), 0, 0).unwrap().and_utc();
            if last_hour == Some(current_hour) {
                continue;
            }
            // First tick after startup: just record current_hour and wait for next boundary.
            if last_hour.is_none() {
                last_hour = Some(current_hour);
                continue;
            }
            last_hour = Some(current_hour);

            let accounts = match auto_trader_db::trading_accounts::list_all(&sfd_pool).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("sfd hourly: failed to list trading accounts: {e}");
                    continue;
                }
            };
            for pac in accounts {
                if pac.account_type != "paper" {
                    continue;
                }
                let exchange = match exchange_from_str(&pac.exchange) {
                    Some(e) => e,
                    None => continue,
                };
                if exchange != Exchange::BitflyerCfd {
                    continue;
                }
                let open_trades = match auto_trader_db::trades::get_open_trades_by_account(
                    &sfd_pool,
                    pac.id,
                )
                .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("sfd hourly: list open trades failed for {}: {e}", pac.name);
                        continue;
                    }
                };
                let fx_key =
                    FeedKey::new(Exchange::BitflyerCfd, auto_trader_core::types::Pair::new("FX_BTC_JPY"));
                let spot_key =
                    FeedKey::new(Exchange::BitflyerCfd, auto_trader_core::types::Pair::new("BTC_JPY"));
                let fx_ba = sfd_price_store.latest_bid_ask(&fx_key).await;
                let spot_ba = sfd_price_store.latest_bid_ask(&spot_key).await;
                let (fx_mid, spot_mid) = match (fx_ba, spot_ba) {
                    (Some((b1, a1)), Some((b2, a2))) => (
                        (b1 + a1) / Decimal::from(2),
                        (b2 + a2) / Decimal::from(2),
                    ),
                    _ => {
                        tracing::warn!(
                            "sfd hourly: skipping {} ({}) — missing FX or spot tick",
                            pac.name,
                            pac.id
                        );
                        continue;
                    }
                };
                for trade in &open_trades {
                    let notional = trade.entry_price * trade.quantity;
                    let fee = sfd::compute_hourly_sfd(sfd::SfdContext {
                        fx_price: fx_mid,
                        spot_price: spot_mid,
                        position_notional: notional,
                        direction: trade.direction,
                    });
                    if fee.is_zero() {
                        continue;
                    }
                    let result = async {
                        let mut tx = sfd_pool.begin().await?;
                        let applied = auto_trader_db::trades::apply_sfd_fee(
                            &mut tx, pac.id, trade.id, fee,
                        )
                        .await?;
                        tx.commit().await?;
                        anyhow::Ok(applied)
                    }
                    .await;
                    match result {
                        Ok(Some(_)) => {
                            tracing::info!(
                                "sfd applied: trade={} fee={} (notional={})",
                                trade.id, fee, notional
                            );
                        }
                        Ok(None) => {
                            tracing::debug!("sfd skip: trade {} closed mid-tick", trade.id);
                        }
                        Err(e) => {
                            tracing::error!("sfd apply failed for trade {}: {e}", trade.id);
                        }
                    }
                }
            }
        }
    });
```

- [ ] **Step 3: Wire `sfd_handle` into the existing task management**

`overnight_handle` 等が既存の `tokio::select! { ... }` や `tokio::join!` などで管理されている場合は、同じ場所に `sfd_handle` を追加する。

```bash
grep -n "overnight_handle\b" crates/app/src/main.rs
```

検索結果に基づいて `sfd_handle` を同じ pattern で扱う (await / select に追加 or 単純に `let _ = sfd_handle;` で keep alive)。

- [ ] **Step 4: Build to verify imports**

```bash
cargo build -p auto-trader 2>&1 | tail -20
```

Expected: 成功。`use chrono::Timelike;` が必要なら `.hour()` メソッド呼び出しで unresolved になるので追加する (main.rs 冒頭の `use` セクション)。

- [ ] **Step 5: Commit (test は Task 7 で integration test と合わせて検証)**

```bash
git add crates/app/src/main.rs
git commit -m "feat(app): hourly SFD accrual job for paper bitFlyer accounts"
```

---

### Task 7: Integration test `phase3_sfd_paper_accrual.rs`

**Files:**
- Create: `crates/integration-tests/tests/phase3_sfd_paper_accrual.rs`

**注**: hourly cron 自体を end-to-end で動かすのは難しい (60s interval を実時間で待つ必要)。このテストは `apply_sfd_fee` + `compute_hourly_sfd` の combination を直接呼ぶ statement-level integration とする。cron loop 自体の wiring は Task 6 の手動目視 + Task 8 の test-all 全体実行で担保する。

- [ ] **Step 1: Write the integration test file**

```rust
//! Phase 3: paper bitFlyer SFD accrual の DB レベル統合テスト。
//!
//! `compute_hourly_sfd` で fee を算出 → `apply_sfd_fee` で DB に反映する
//! 一連のフローを実 DB で検証。hourly cron 自体の wiring は main.rs に
//! 任せ、ここでは fee 算出 + DB 反映の組み合わせのみテストする。
//!
//! - paper bitFlyer + 10% divergence Long → fees 増加、balance 減少、event 記録
//! - paper bitFlyer + 4% divergence → SFD = 0 (apply_sfd_fee 呼ばれない)
//! - paper bitFlyer + 10% Short, FX>spot → fees 減少、balance 増加 (受け取り)
//! - live bitFlyer はこの job 経路の対象外 (test では account_type フィルタを示す)
//! - GMO FX 対象外

use auto_trader_core::sfd::{compute_hourly_sfd, SfdContext};
use auto_trader_core::types::Direction;
use auto_trader_db::trades::{apply_sfd_fee, get_trade_by_id};
use auto_trader_integration_tests::helpers::db::seed_trading_account;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

async fn insert_open_trade(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    pair: &str,
    direction: Direction,
    entry_price: Decimal,
    quantity: Decimal,
) -> Uuid {
    let trade_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO trades
               (id, account_id, strategy_name, pair, exchange, direction,
                entry_price, stop_loss, take_profit, quantity, leverage,
                fees, entry_at, status)
           VALUES ($1, $2, 'test_strat', $3, 'bitflyer_cfd', $4,
                   $5, $5 - 1, $5 + 1, $6, 2,
                   0, NOW(), 'open')"#,
    )
    .bind(trade_id)
    .bind(account_id)
    .bind(pair)
    .bind(match direction {
        Direction::Long => "long",
        Direction::Short => "short",
    })
    .bind(entry_price)
    .bind(quantity)
    .execute(pool)
    .await
    .expect("insert trade");
    trade_id
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_bitflyer_10pct_long_pays_hourly_sfd(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool, "sfd_accr_long", "paper", "bitflyer_cfd", "test_strat", 1_000_000,
    )
    .await;
    let trade_id = insert_open_trade(
        &pool, account_id, "FX_BTC_JPY", Direction::Long, dec!(36000), dec!(0.01),
    )
    .await;

    // 10% divergence (FX > spot), Long → 払う
    let fee = compute_hourly_sfd(SfdContext {
        fx_price: dec!(110),
        spot_price: dec!(100),
        position_notional: dec!(36000) * dec!(0.01), // 360
        direction: Direction::Long,
    });
    assert!(fee > Decimal::ZERO);

    let mut tx = pool.begin().await.unwrap();
    let new_balance = apply_sfd_fee(&mut tx, account_id, trade_id, fee)
        .await
        .unwrap()
        .expect("Some");
    tx.commit().await.unwrap();

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, fee);
    assert_eq!(new_balance, dec!(1_000_000) - fee);
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_bitflyer_below_threshold_yields_zero_sfd(pool: sqlx::PgPool) {
    // 4% divergence → SFD = 0
    let fee = compute_hourly_sfd(SfdContext {
        fx_price: dec!(104),
        spot_price: dec!(100),
        position_notional: dec!(360),
        direction: Direction::Long,
    });
    assert_eq!(fee, Decimal::ZERO);
    // job 側は fee.is_zero() で apply_sfd_fee を呼ばないので、ここでも DB 操作しない。
    // pool は接続確認のみ。
    let _ = pool;
}

#[sqlx::test(migrations = "../../migrations")]
async fn paper_bitflyer_10pct_short_receives_sfd(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool, "sfd_accr_short", "paper", "bitflyer_cfd", "test_strat", 1_000_000,
    )
    .await;
    let trade_id = insert_open_trade(
        &pool, account_id, "FX_BTC_JPY", Direction::Short, dec!(36000), dec!(0.01),
    )
    .await;

    // 10% divergence (FX > spot), Short → 受け取る
    let fee = compute_hourly_sfd(SfdContext {
        fx_price: dec!(110),
        spot_price: dec!(100),
        position_notional: dec!(360),
        direction: Direction::Short,
    });
    assert!(fee < Decimal::ZERO);

    let mut tx = pool.begin().await.unwrap();
    let new_balance = apply_sfd_fee(&mut tx, account_id, trade_id, fee)
        .await
        .unwrap()
        .expect("Some");
    tx.commit().await.unwrap();

    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, fee);  // 負値
    assert!(trade.fees < Decimal::ZERO);
    assert_eq!(new_balance, dec!(1_000_000) - fee);  // balance increased
    assert!(new_balance > dec!(1_000_000));
}

#[sqlx::test(migrations = "../../migrations")]
async fn apply_sfd_fee_returns_none_when_trade_closed(pool: sqlx::PgPool) {
    // CAS skip: trade が closed なら apply_sfd_fee は Ok(None) を返し、
    // 副作用なし (fees / balance / events に変化なし)。
    let account_id = seed_trading_account(
        &pool, "sfd_accr_closed", "paper", "bitflyer_cfd", "test_strat", 1_000_000,
    )
    .await;
    let trade_id = insert_open_trade(
        &pool, account_id, "FX_BTC_JPY", Direction::Long, dec!(36000), dec!(0.01),
    )
    .await;
    sqlx::query("UPDATE trades SET status='closed' WHERE id=$1")
        .bind(trade_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let result = apply_sfd_fee(&mut tx, account_id, trade_id, dec!(15))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(result.is_none(), "closed trade must skip apply_sfd_fee");

    // fees 変化なし
    let trade = get_trade_by_id(&pool, trade_id).await.unwrap().unwrap();
    assert_eq!(trade.fees, Decimal::ZERO);
}

#[sqlx::test(migrations = "../../migrations")]
async fn account_event_row_recorded_with_sfd_fee_type(pool: sqlx::PgPool) {
    let account_id = seed_trading_account(
        &pool, "sfd_accr_event", "paper", "bitflyer_cfd", "test_strat", 1_000_000,
    )
    .await;
    let trade_id = insert_open_trade(
        &pool, account_id, "FX_BTC_JPY", Direction::Long, dec!(36000), dec!(0.01),
    )
    .await;

    let mut tx = pool.begin().await.unwrap();
    apply_sfd_fee(&mut tx, account_id, trade_id, dec!(15))
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
    assert_eq!(amount, dec!(-15)); // 払い時 amount は -fee
    assert_eq!(evt_type, "sfd_fee");
}
```

- [ ] **Step 2: Run integration test**

```bash
cargo test -p auto-trader-integration-tests --test phase3_sfd_paper_accrual 2>&1 | tail -15
```

Expected: 5 tests PASS (10pct_long_pays / below_threshold_zero / 10pct_short_receives / closed_trade_skip / event_row_recorded)。

- [ ] **Step 3: Commit**

```bash
git add crates/integration-tests/tests/phase3_sfd_paper_accrual.rs
git commit -m "test: phase3_sfd_paper_accrual (compute + apply DB integration)"
```

---

### Task 8: test-all → simplify → code-review → PR

- [ ] **Step 1: Run the full test suite**

```bash
./scripts/test-all.sh 2>&1 | tail -15
```

Expected: ALL GREEN.

- [ ] **Step 2: Self-review with `simplify` skill**

```
Invoke skill: simplify
```

3 並列 review agent を起動。findings を inline 修正。

- [ ] **Step 3: Self-review with `reviewer.md` 5 観点**

特にチェック:
- **Reliability**: spot tick が長時間 stale な場合に SFD を skip するか (現状 spot_ba が `None` なら skip)。`PriceStore.last_tick_age` で freshness 確認すべきか?
- **Performance**: hourly job が 60s interval で hour boundary check するのは無駄ない (cheap)
- **Architecture**: `apply_sfd_fee` の符号規約 (`amount = -fee_amount`) が `overnight_fee` と一貫

- [ ] **Step 4: Push branch (deny rule のためユーザに依頼)**

```bash
git push -u origin feat/sfd-paper-spot
```

(deny されたらユーザに手動 push 依頼)

- [ ] **Step 5: Open PR**

```bash
gh pr create --title "feat(app): bitFlyer SFD paper accrual (close the last paper=live gap)" --body "$(cat <<'EOF'
## Summary
- paper account でも bitFlyer Crypto CFD の SFD を hourly accrual して \`Trade.fees\` に積算
- `core::sfd::sfd_daily_rate` (公式階段 hardcode) + `compute_hourly_sfd` 追加
- 既存 bitFlyer WS が FX_BTC_JPY subscribe 時に BTC_JPY (現物) を自動 subscribe し PriceStore に流す
- 新規 `db::trades::apply_sfd_fee` (`apply_overnight_fee` と同パターン、符号両対応)
- 新規 hourly cron task (paper bitflyer のみ、live は PR #91 の `fetch_close_sfd` のまま → double counting なし)

これで **paper=live contract** の最後の例外を解消。残るはレイテンシ起因 slippage / exchange-side rejection (= ユーザが除外したもの) のみ。

## Docs
- Spec: \`docs/superpowers/specs/2026-05-18-sfd-paper-spot-design.md\`
- Plan: \`docs/superpowers/plans/2026-05-18-sfd-paper-spot.md\`

## Test plan
- [x] \`./scripts/test-all.sh\` ALL GREEN
- [x] core unit: sfd_daily_rate 6 ケース + compute_hourly_sfd 6 ケース
- [x] db unit: apply_sfd_fee 3 ケース (positive / negative / closed)
- [x] integration: phase3_sfd_paper_accrual 4 ケース

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 6: Copilot review loop**

```bash
gh pr edit <PR#> --add-reviewer copilot-pull-request-reviewer
```

ScheduleWakeup 270s → comments 確認 → 対応 → push → re-request。指摘ゼロ / 軽微 suggestion のみまで継続。
