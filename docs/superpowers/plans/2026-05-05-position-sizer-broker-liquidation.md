# Position Sizer Broker-Specific Liquidation-Aware Sizing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the hardcoded `maintenance_margin_rate = 0.50` in `PositionSizer` with a per-exchange `liquidation_margin_level` driven by `config/default.toml`, applying the formula `max_alloc = 1 / (Y + leverage × stop_loss_pct)` so that the SL hit point coincides exactly with the broker's liquidation threshold.

**Architecture:** Add `ExchangeMarginConfig` to `AppConfig`, plumb a per-exchange `Decimal` through `Trader::new` to `PositionSizer::calculate_quantity`, and fail the process at startup if any active account points at an exchange without a `liquidation_margin_level` entry.

**Tech Stack:** Rust + sqlx + rust_decimal + serde + toml. `cargo test` for verification. PostgreSQL via Docker compose for integration.

**Spec:** `docs/superpowers/specs/2026-05-05-position-sizer-broker-liquidation-design.md`

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/core/src/config.rs` | `AppConfig` schema | **Modify** — add `ExchangeMarginConfig` struct + `exchange_margin: HashMap<String, ExchangeMarginConfig>` field |
| `crates/executor/src/position_sizer.rs` | Sizing formula | **Modify** — drop hardcoded 0.50, add `liquidation_margin_level: Decimal` arg, apply `1/(Y + L×s)` |
| `crates/executor/src/trader.rs` | Trader executor | **Modify** — add `liquidation_margin_level: Decimal` field + extra `Trader::new` arg, pass through to sizer |
| `crates/app/src/main.rs` | Startup wiring | **Modify** — build `HashMap<Exchange, Decimal>` from config, fail at startup if any active account's exchange is missing, pass per-account value to each `UnifiedTrader::new` |
| `config/default.toml` | Default config | **Modify** — add `[exchange_margin.bitflyer_cfd]`, `[exchange_margin.gmo_fx]`, and `[exchange_margin.oanda]` (placeholder for the unused-but-still-present `Exchange::Oanda` variant; mirrored into the test fixture for the same reason) |
| `crates/executor/tests/trader_test.rs` | Trader unit tests | **Modify** — pass new arg |
| `crates/integration-tests/tests/phase3_close_flow.rs` | Integration tests | **Modify** — pass new arg (3 sites) |
| `crates/integration-tests/tests/phase3_execution.rs` | Integration tests | **Modify** — pass new arg |
| `crates/integration-tests/tests/phase3_execution_flow.rs` | Integration tests | **Modify** — pass new arg (4 sites) |
| `crates/integration-tests/tests/phase3_integrity.rs` | Integration tests | **Modify** — pass new arg |
| `crates/integration-tests/tests/phase3_monitoring.rs` | Integration tests | **Modify** — pass new arg |
| `crates/integration-tests/fixtures/config_valid.toml` | Test fixture | **Modify** — add exchange_margin sections |
| `crates/integration-tests/tests/phase1_config.rs` (or new file) | Startup config validation | **Add new test** — fail-closed when section missing |

---

## Task 1: Add `ExchangeMarginConfig` to `AppConfig`

**Files:**
- Modify: `crates/core/src/config.rs:1-30` (struct), test block at bottom

- [ ] **Step 1.1: Write failing deserialization test**

Add test to bottom of `crates/core/src/config.rs` (existing `#[cfg(test)] mod tests` — append):

```rust
    #[test]
    fn parses_exchange_margin_section() {
        let toml_str = r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = ["USD_JPY"]
crypto = ["FX_BTC_JPY"]

[exchange_margin.bitflyer_cfd]
liquidation_margin_level = 0.50

[exchange_margin.gmo_fx]
liquidation_margin_level = 1.00
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.exchange_margin.get("bitflyer_cfd").map(|c| c.liquidation_margin_level),
            Some(rust_decimal_macros::dec!(0.50))
        );
        assert_eq!(
            config.exchange_margin.get("gmo_fx").map(|c| c.liquidation_margin_level),
            Some(rust_decimal_macros::dec!(1.00))
        );
    }

    #[test]
    fn exchange_margin_defaults_to_empty_when_missing() {
        let toml_str = r#"
[vegapunk]
endpoint = "http://x"
schema = "y"
[database]
url = "postgresql://x"
[monitor]
interval_secs = 60
[pairs]
fx = []
crypto = []
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(config.exchange_margin.is_empty());
    }
```

- [ ] **Step 1.2: Run tests and verify failure**

```bash
cargo test -p auto_trader_core config::tests::parses_exchange_margin_section -- --nocapture
cargo test -p auto_trader_core config::tests::exchange_margin_defaults_to_empty_when_missing -- --nocapture
```

Expected: both fail to compile (`exchange_margin` field not found on `AppConfig`).

- [ ] **Step 1.3: Add `ExchangeMarginConfig` struct + `AppConfig.exchange_margin` field**

In `crates/core/src/config.rs`, modify the `AppConfig` struct (line 7-29) by adding a new field at the end of the field list, and add the `ExchangeMarginConfig` struct definition right after `PairConfig`:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub oanda: Option<OandaConfig>,
    #[serde(default)]
    pub bitflyer: Option<BitflyerConfig>,
    pub vegapunk: VegapunkConfig,
    pub database: DatabaseConfig,
    pub monitor: MonitorConfig,
    pub pairs: PairsConfig,
    #[serde(default)]
    pub pair_config: HashMap<String, PairConfig>,
    #[serde(default)]
    pub position_sizing: Option<PositionSizingConfig>,
    #[serde(default)]
    pub strategies: Vec<StrategyConfig>,
    #[serde(default)]
    pub macro_analyst: Option<MacroAnalystConfig>,
    #[serde(default)]
    pub gemini: Option<GeminiConfig>,
    #[serde(default)]
    pub live: Option<LiveConfig>,
    #[serde(default)]
    pub risk: Option<RiskConfig>,
    /// Per-exchange margin settings (TOML key `[exchange_margin.<exchange>]`).
    /// Each entry's `liquidation_margin_level` is the broker's margin-call
    /// threshold expressed as a decimal (e.g., 0.50 for bitFlyer Crypto CFD,
    /// 1.00 for GMOコイン外国為替FX). PositionSizer caps allocation so the
    /// post-SL margin level does not fall below this threshold.
    #[serde(default)]
    pub exchange_margin: HashMap<String, ExchangeMarginConfig>,
}
```

Add right below the `PairConfig` struct (around line 130):

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct ExchangeMarginConfig {
    pub liquidation_margin_level: Decimal,
}
```

- [ ] **Step 1.4: Run tests and verify they pass**

```bash
cargo test -p auto_trader_core config::tests::parses_exchange_margin_section
cargo test -p auto_trader_core config::tests::exchange_margin_defaults_to_empty_when_missing
cargo test -p auto_trader_core
```

Expected: both new tests pass; existing config tests unchanged.

- [ ] **Step 1.5: Commit**

```bash
git add crates/core/src/config.rs
git commit -m "$(cat <<'EOF'
feat(core): add ExchangeMarginConfig to AppConfig

Per-exchange [exchange_margin.<name>] sections expose liquidation_margin_level
to the position sizer. Defaults to empty HashMap when absent, validation of
required entries happens at startup (next commit).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Rewrite `PositionSizer` with new formula

**Files:**
- Modify: `crates/executor/src/position_sizer.rs` (entire file — formula, signature, tests)

This task is atomic — the signature change forces all callers to update simultaneously. We update callers in Tasks 3 and 5.

- [ ] **Step 2.1: Write the new unit tests (replacing old ones)**

Replace the entire `#[cfg(test)] mod tests { ... }` block at the bottom of `crates/executor/src/position_sizer.rs` with the following:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::Pair;
    use rust_decimal_macros::dec;

    fn btc_sizer() -> PositionSizer {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
        PositionSizer::new(min_sizes)
    }

    fn fx_sizer() -> PositionSizer {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
        PositionSizer::new(min_sizes)
    }

    /// gmo_fx (Y=1.0) lev=10, SL=2%, balance=30,000円: max_alloc = 1/(1.0+0.2) ≈ 0.833.
    /// position_value = 30,000 × 10 × 0.833 / 157 = 1591 USD → truncated to 1591 (min_lot=1).
    #[test]
    fn gmo_fx_loose_sl_caps_at_alloc_below_one() {
        let qty = fx_sizer().calculate_quantity(
            &Pair::new("USD_JPY"),
            dec!(30000),
            dec!(157),
            dec!(10),
            dec!(1.0),
            dec!(0.02),
            dec!(1.00),
        );
        assert_eq!(qty, Some(dec!(1591)));
    }

    /// gmo_fx (Y=1.0) lev=10, SL=0.5%: max_alloc = 1/(1.0+0.05) ≈ 0.952.
    /// 30,000 × 10 × 0.952 / 157 = 1819 USD → 1819.
    #[test]
    fn gmo_fx_tight_sl_higher_allocation() {
        let qty = fx_sizer().calculate_quantity(
            &Pair::new("USD_JPY"),
            dec!(30000),
            dec!(157),
            dec!(10),
            dec!(1.0),
            dec!(0.005),
            dec!(1.00),
        );
        assert_eq!(qty, Some(dec!(1819)));
    }

    /// bitflyer_cfd (Y=0.5) lev=2, SL=2%: max_alloc = 1/(0.5+0.04) ≈ 1.85 → cap at allocation_pct=1.0.
    /// 30,000 × 2 × 1.0 / 12,500,000 = 0.0048 → truncated to 0.004 (multiple of min_lot=0.001).
    #[test]
    fn bitflyer_cfd_typical_sl_uses_full_allocation() {
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(30000),
            dec!(12500000),
            dec!(2),
            dec!(1.0),
            dec!(0.02),
            dec!(0.50),
        );
        assert_eq!(qty, Some(dec!(0.004)));
    }

    /// At lev=10, SL=10%, Y=1.0: max_alloc = 1/(1.0+1.0) = 0.5 → forces under-allocation.
    /// 30,000 × 10 × 0.5 / 157 = 955 USD.
    #[test]
    fn lc_constraint_binds_at_high_leverage_and_wide_sl() {
        let qty = fx_sizer().calculate_quantity(
            &Pair::new("USD_JPY"),
            dec!(30000),
            dec!(157),
            dec!(10),
            dec!(1.0),
            dec!(0.10),
            dec!(1.00),
        );
        assert_eq!(qty, Some(dec!(955)));
    }

    /// Caller-supplied allocation_pct dominates when smaller than the LC cap.
    /// bitflyer_cfd, alloc=0.5 → 30,000 × 2 × 0.5 / 12,500,000 = 0.0024 → 0.002.
    #[test]
    fn allocation_pct_dominates_when_smaller_than_max_alloc() {
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(30000),
            dec!(12500000),
            dec!(2),
            dec!(0.5),
            dec!(0.02),
            dec!(0.50),
        );
        assert_eq!(qty, Some(dec!(0.002)));
    }

    /// liquidation_margin_level <= 0 is treated as a configuration bug.
    #[test]
    fn rejects_zero_or_negative_liquidation_margin_level() {
        let s = fx_sizer();
        let p = Pair::new("USD_JPY");
        assert_eq!(
            s.calculate_quantity(&p, dec!(30000), dec!(157), dec!(10), dec!(1.0), dec!(0.02), dec!(0)),
            None
        );
        assert_eq!(
            s.calculate_quantity(&p, dec!(30000), dec!(157), dec!(10), dec!(1.0), dec!(0.02), dec!(-0.5)),
            None
        );
    }

    /// Existing input validations preserved.
    #[test]
    fn rejects_zero_or_negative_inputs() {
        let s = fx_sizer();
        let p = Pair::new("USD_JPY");
        // zero balance
        assert_eq!(
            s.calculate_quantity(&p, dec!(0), dec!(157), dec!(10), dec!(1.0), dec!(0.02), dec!(1.00)),
            None
        );
        // zero price
        assert_eq!(
            s.calculate_quantity(&p, dec!(30000), dec!(0), dec!(10), dec!(1.0), dec!(0.02), dec!(1.00)),
            None
        );
        // zero leverage
        assert_eq!(
            s.calculate_quantity(&p, dec!(30000), dec!(157), dec!(0), dec!(1.0), dec!(0.02), dec!(1.00)),
            None
        );
        // zero allocation
        assert_eq!(
            s.calculate_quantity(&p, dec!(30000), dec!(157), dec!(10), dec!(0), dec!(0.02), dec!(1.00)),
            None
        );
        // > 100% allocation rejected
        assert_eq!(
            s.calculate_quantity(&p, dec!(30000), dec!(157), dec!(10), dec!(1.5), dec!(0.02), dec!(1.00)),
            None
        );
        // zero stop loss
        assert_eq!(
            s.calculate_quantity(&p, dec!(30000), dec!(157), dec!(10), dec!(1.0), dec!(0), dec!(1.00)),
            None
        );
    }

    /// Truncation to min_lot still rejects when result < one lot.
    #[test]
    fn rejects_when_account_too_small_for_one_min_lot() {
        // bitflyer_cfd, balance=5,000円 with BTC ~12.5M and lev=2: full alloc
        // qty = 5000 × 2 × 1.0 / 12,500,000 = 0.0008 → below 0.001 min lot
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(5000),
            dec!(12500000),
            dec!(2),
            dec!(1.0),
            dec!(0.02),
            dec!(0.50),
        );
        assert_eq!(qty, None);
    }

    /// Property: applying max_alloc places the post-SL margin level exactly at Y.
    ///   margin_level = (1 - L × a × s) / a; with a = 1 / (Y + L × s), this equals Y.
    #[test]
    fn post_sl_margin_level_equals_threshold_invariant() {
        let cases = [
            (dec!(10), dec!(0.005), dec!(1.00)),
            (dec!(10), dec!(0.02), dec!(1.00)),
            (dec!(10), dec!(0.10), dec!(1.00)),
            (dec!(2), dec!(0.02), dec!(0.50)),
            (dec!(2), dec!(0.05), dec!(0.50)),
        ];
        for (lev, sl, y) in cases {
            // a = 1 / (Y + L × s), bounded by 1.0 caller-side.
            let a_unbounded = Decimal::ONE / (y + lev * sl);
            // Skip cases where the cap is the binding constraint (no LC pressure).
            if a_unbounded >= Decimal::ONE {
                continue;
            }
            // post-SL margin level
            let ml = (Decimal::ONE - lev * a_unbounded * sl) / a_unbounded;
            // Allow 1 bp tolerance for Decimal rounding.
            let diff = (ml - y).abs();
            assert!(
                diff < dec!(0.0001),
                "lev={lev}, sl={sl}, y={y}: margin level {ml} != threshold (diff={diff})"
            );
        }
    }
}
```

- [ ] **Step 2.2: Run tests, verify all of them fail**

```bash
cargo test -p auto_trader_executor position_sizer:: 2>&1 | tail -30
```

Expected: compile errors (`calculate_quantity` takes 6 args, called with 7) or assertion failures.

- [ ] **Step 2.3: Update `PositionSizer` doc comment + signature + body**

Replace `crates/executor/src/position_sizer.rs` lines 1-100 (everything before `#[cfg(test)]`) with:

```rust
use auto_trader_core::types::Pair;
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Position sizer: converts Signal.allocation_pct → concrete quantity.
///
/// Sizing strategy: **invest the maximum amount that keeps the post-SL
/// margin level at or above the broker's liquidation threshold**.
///
///   max_alloc = 1 / (Y + leverage × stop_loss_pct)
///   risk_alloc = min(max_alloc, allocation_pct)
///
/// `Y` is the broker's liquidation margin level threshold supplied by the
/// caller (resolved per-exchange from `[exchange_margin.<name>]` config).
/// At `risk_alloc = max_alloc`, the SL hit point coincides with the
/// liquidation line — i.e., a directly-hit SL closes at exactly margin
/// level Y, so any move further than the SL is impossible because the SL
/// fires first.
///
/// Example: bitflyer_cfd (Y=0.5, lev=2), SL=2%
///   max_alloc = 1 / (0.5 + 0.04) = 1.85 → capped at allocation_pct (1.0)
///
/// Example: gmo_fx (Y=1.0, lev=10), SL=2%
///   max_alloc = 1 / (1.0 + 0.2) = 0.833 → 83.3% of balance as margin
pub struct PositionSizer {
    min_order_sizes: HashMap<Pair, Decimal>,
}

impl PositionSizer {
    pub fn new(min_order_sizes: HashMap<Pair, Decimal>) -> Self {
        Self { min_order_sizes }
    }

    /// Compute the trade quantity. Returns None when the result would
    /// be below the per-pair `min_order_size`, when any input is non-positive,
    /// or when `liquidation_margin_level` is non-positive (configuration bug).
    ///
    /// `allocation_pct` must be in (0, 1]. Values outside that range
    /// are treated as bugs and rejected (returns None) — the sizer
    /// does not silently clamp.
    ///
    /// `stop_loss_pct` is the stop-loss distance as a fraction of fill price
    /// (e.g., 0.005 = 0.5% distance).
    ///
    /// `liquidation_margin_level` is the broker's margin-call threshold as
    /// a decimal (e.g., 0.50 for bitFlyer Crypto CFD's 50%, 1.00 for GMO
    /// 外国為替FX's 100%).
    #[allow(clippy::too_many_arguments)]
    pub fn calculate_quantity(
        &self,
        pair: &Pair,
        balance: Decimal,
        entry_price: Decimal,
        leverage: Decimal,
        allocation_pct: Decimal,
        stop_loss_pct: Decimal,
        liquidation_margin_level: Decimal,
    ) -> Option<Decimal> {
        if balance <= Decimal::ZERO
            || entry_price <= Decimal::ZERO
            || leverage <= Decimal::ZERO
            || allocation_pct <= Decimal::ZERO
            || allocation_pct > Decimal::ONE
            || stop_loss_pct <= Decimal::ZERO
            || liquidation_margin_level <= Decimal::ZERO
        {
            return None;
        }

        // SL ヒット時の維持率 = (1 - L × a × s) / a ≥ Y を解いて
        //   a ≤ 1 / (Y + L × s)
        let max_alloc =
            Decimal::ONE / (liquidation_margin_level + leverage * stop_loss_pct);
        let risk_alloc = max_alloc.min(allocation_pct);

        // Mechanical sizing: apply leverage and risk-adjusted allocation, divide by price.
        let raw_qty = balance * leverage * risk_alloc / entry_price;

        let min_size = self
            .min_order_sizes
            .get(pair)
            .copied()
            .unwrap_or(Decimal::ZERO);

        if min_size > Decimal::ZERO {
            let truncated = (raw_qty / min_size).floor() * min_size;
            if truncated < min_size {
                return None;
            }
            Some(truncated)
        } else if raw_qty > Decimal::ZERO {
            Some(raw_qty)
        } else {
            None
        }
    }
}
```

- [ ] **Step 2.4: Run tests, verify all PositionSizer tests pass**

```bash
cargo test -p auto_trader_executor position_sizer::
```

Expected: all 9 tests pass.

- [ ] **Step 2.5: DO NOT commit yet — `Trader` and callers will not compile**

The whole `auto_trader_executor` and `auto_trader_app` crates plus integration tests are now broken because `Trader` calls `calculate_quantity` with 6 args. Tasks 3-5 fix this. Once Task 5 passes, commit Tasks 2-5 atomically.

---

## Task 3: Plumb `liquidation_margin_level` through `Trader`

**Files:**
- Modify: `crates/executor/src/trader.rs:71-160` (struct + new) and `:670-688` (sizer call)

- [ ] **Step 3.1: Add field to `Trader` struct**

In `crates/executor/src/trader.rs`, in the `pub struct Trader { ... }` block (around line 60-86), add a new field at the bottom of the struct (just before the closing `}`):

```rust
pub struct Trader {
    pool: PgPool,
    exchange: Exchange,
    account_id: Uuid,
    /// Cached at construction time so every Slack notification shows the
    /// human-readable account name rather than the raw UUID.
    account_name: String,
    api: Arc<dyn ExchangeApi>,
    price_store: Arc<PriceStore>,
    notifier: Arc<Notifier>,
    /// Shared PositionSizer — pre-built at startup, every per-tick task
    /// holds an `Arc::clone` instead of reconstructing the inner
    /// HashMap on every signal/SL/TP check.
    position_sizer: Arc<PositionSizer>,
    /// Broker liquidation threshold (証拠金維持率の下限). Resolved from
    /// `[exchange_margin.<exchange>]` at startup and held per-Trader.
    liquidation_margin_level: Decimal,
    dry_run: bool,
    /// Timeout passed to `poll_executions` for both open and close fills.
    poll_timeout: Duration,
}
```

- [ ] **Step 3.2: Update `Trader::new` signature and body**

In the same file, modify the `pub fn new(...)` definition (around line 130-156) to take `liquidation_margin_level: Decimal` after `position_sizer`:

```rust
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        exchange: Exchange,
        account_id: Uuid,
        account_name: String,
        api: Arc<dyn ExchangeApi>,
        price_store: Arc<PriceStore>,
        notifier: Arc<Notifier>,
        position_sizer: Arc<PositionSizer>,
        liquidation_margin_level: Decimal,
        dry_run: bool,
    ) -> Self {
        Self {
            pool,
            exchange,
            account_id,
            account_name,
            api,
            price_store,
            notifier,
            position_sizer,
            liquidation_margin_level,
            dry_run,
            poll_timeout: Duration::from_secs(5),
        }
    }
```

- [ ] **Step 3.3: Pass the value into the sizer call**

Find the `sizer.calculate_quantity(...)` call inside `execute()` (around line 678-688) and add `self.liquidation_margin_level` as the final argument:

```rust
        let quantity = sizer
            .calculate_quantity(
                &signal.pair,
                balance,
                hint_price,
                leverage,
                signal.allocation_pct,
                signal.stop_loss_pct,
                self.liquidation_margin_level,
            )
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "account balance too small to open minimum order for {}",
                    signal.pair
                )
            })?;
```

- [ ] **Step 3.4: Verify Trader compiles in isolation (other crates still broken)**

```bash
cargo check -p auto_trader_executor 2>&1 | tail -20
```

Expected: `auto_trader_executor` compiles. (Tests still broken — fixed in Task 4 with one additional sweep through executor's own tests.)

- [ ] **Step 3.5: Update `crates/executor/tests/trader_test.rs`**

Find every `Trader::new(...)` call in this file. Insert a new line `dec!(0.50),` (for bitflyer_cfd) or `dec!(1.00),` (for gmo_fx) right after the `position_sizer.clone(),` argument and before `dry_run`.

If unsure which exchange a given test uses, grep for the `Exchange::` value passed in the same call. Use the matching liquidation margin level (`dec!(0.50)` for `Exchange::BitflyerCfd`, `dec!(1.00)` for `Exchange::GmoFx`).

Add to the file's imports if not already present:

```rust
use rust_decimal_macros::dec;
```

- [ ] **Step 3.6: Verify executor crate builds + tests compile**

```bash
cargo test -p auto_trader_executor --no-run 2>&1 | tail -20
```

Expected: clean build. We do NOT run the tests yet (some integration tests under `crates/integration-tests/` still won't compile, and trader_test.rs may have DB dependencies).

---

## Task 4: Update integration test callers

**Files:** All under `crates/integration-tests/tests/`:
- Modify: `phase3_close_flow.rs` (3 `Trader::new` sites)
- Modify: `phase3_execution.rs` (1 site)
- Modify: `phase3_execution_flow.rs` (4 sites)
- Modify: `phase3_integrity.rs` (1 site)
- Modify: `phase3_monitoring.rs` (1 site)

Same mechanical change as Step 3.5 in each file. Use `dec!(0.50)` if the test sets up `Exchange::BitflyerCfd`, `dec!(1.00)` for `Exchange::GmoFx`. Add `use rust_decimal_macros::dec;` to file imports if missing.

- [ ] **Step 4.1: Update `phase3_close_flow.rs`**

Locate each `Trader::new(...)` call (lines 58, 274, 295, 409). Add the appropriate `dec!(0.50)` or `dec!(1.00)` argument before `dry_run` per the exchange used in that test. Add `use rust_decimal_macros::dec;` near the other `use` statements at the top if absent.

- [ ] **Step 4.2: Update `phase3_execution.rs`**

Locate `Trader::new(...)` at line 129. Add the new arg.

- [ ] **Step 4.3: Update `phase3_execution_flow.rs`**

Locate `Trader::new(...)` calls at lines 203, 274, 459, 526. Update each.

- [ ] **Step 4.4: Update `phase3_integrity.rs`**

Locate `Trader::new(...)` at line 56.

- [ ] **Step 4.5: Update `phase3_monitoring.rs`**

Locate `Trader::new(...)` at line 57.

- [ ] **Step 4.6: Verify integration-tests crate compiles**

```bash
cargo test -p auto_trader_integration_tests --no-run 2>&1 | tail -20
```

Expected: clean build.

---

## Task 5: Update `main.rs` startup wiring

**Files:**
- Modify: `crates/app/src/main.rs:583-620` (sizer construction area, build per-exchange map, fail-closed validation)
- Modify: `crates/app/src/main.rs:925`, `:1184`, `:1361` (each `UnifiedTrader::new` call)

- [ ] **Step 5.1: Build per-exchange `HashMap<Exchange, Decimal>` and validate against active accounts**

In `crates/app/src/main.rs`, between the existing `pair_configs` block (line ~591) and the `shared_position_sizer` block (line ~597), add:

```rust
    // Per-exchange liquidation margin levels — required for any active account.
    // Fail-closed: if config lacks an entry for an exchange used by an active
    // account, abort startup before any trading task spawns.
    let exchange_liquidation_levels: Arc<HashMap<auto_trader_core::types::Exchange, Decimal>> = {
        let active_accounts = auto_trader_db::trading_accounts::list_accounts(&pool).await?;
        let mut required: std::collections::HashSet<auto_trader_core::types::Exchange> =
            std::collections::HashSet::new();
        for acct in &active_accounts {
            required.insert(acct.exchange);
        }
        let mut map: HashMap<auto_trader_core::types::Exchange, Decimal> = HashMap::new();
        for (key, cfg) in config.exchange_margin.iter() {
            match key.parse::<auto_trader_core::types::Exchange>() {
                Ok(ex) => {
                    map.insert(ex, cfg.liquidation_margin_level);
                }
                Err(e) => {
                    anyhow::bail!(
                        "config: [exchange_margin.{key}] is not a recognised exchange: {e}"
                    );
                }
            }
        }
        let missing: Vec<_> = required
            .iter()
            .filter(|ex| !map.contains_key(*ex))
            .collect();
        if !missing.is_empty() {
            anyhow::bail!(
                "config: [exchange_margin.<name>] missing for active accounts: {:?}. \
                 Add `liquidation_margin_level` for each.",
                missing
            );
        }
        Arc::new(map)
    };
```

NOTE: This task assumes `auto_trader_db::trading_accounts::list_accounts(&pool)` exists and returns all accounts. If the function name differs, locate the equivalent (search for existing `list_*` calls on `trading_accounts` and reuse). Also pre-check that `Exchange` implements `FromStr`.

- [ ] **Step 5.2: Verify `Exchange::FromStr` and `list_accounts` exist**

```bash
grep -n 'impl FromStr for Exchange\|fn from_str' crates/core/src/types.rs | head
grep -n 'pub async fn list_accounts\|pub async fn list_all' crates/db/src/trading_accounts.rs | head
```

Expected: `Exchange::FromStr` defined (we already saw it at types.rs:52). For `list_accounts`, if missing, search `trading_accounts.rs` for any existing fetch-all helper (e.g. `list_all` / `all` / `get_all`) and use that name; if no helper exists, add a minimal `pub async fn list_all_accounts` that returns `Vec<TradingAccount>` from a `SELECT * FROM trading_accounts` query, then use it. (Keep this addition self-contained in `trading_accounts.rs`.)

- [ ] **Step 5.3: Pass per-account `liquidation_margin_level` to each `UnifiedTrader::new`**

Three `UnifiedTrader::new(...)` sites exist (lines ~925, ~1184, ~1361). Each is inside a closure/loop where `trade.exchange` or the account's exchange is in scope. Add the resolution + arg in each call:

For the call at ~925 (close-position path inside crypto monitor):

```rust
                    let trader = UnifiedTrader::new(
                        crypto_monitor_pool.clone(),
                        trade.exchange,
                        account_id,
                        account_name,
                        api,
                        crypto_monitor_price_store.clone(),
                        crypto_monitor_notifier.clone(),
                        crypto_monitor_position_sizer.clone(),
                        *exchange_liquidation_levels
                            .get(&trade.exchange)
                            .expect("exchange_liquidation_levels validated at startup"),
                        dry_run,
                    );
```

(Capture `exchange_liquidation_levels` into the surrounding `tokio::spawn` closure by `let exchange_liquidation_levels = exchange_liquidation_levels.clone();` near the other `_clone()` setup before the spawn — search for `crypto_monitor_position_sizer = shared_position_sizer.clone();` and add `let crypto_monitor_exchange_liquidation_levels = exchange_liquidation_levels.clone();` next to it; rename in this call accordingly.)

Apply the analogous pattern at the other two call sites (`exit_position_sizer` clone area near line 1122 → add an `exit_exchange_liquidation_levels`; `executor_position_sizer` near line 1248 → add `executor_exchange_liquidation_levels`). At each `UnifiedTrader::new`, look up by the relevant exchange variable in scope (`trade.exchange` for close-path, `signal.exchange` or account-derived for open-path).

- [ ] **Step 5.4: Verify the binary builds**

```bash
cargo build -p auto_trader 2>&1 | tail -20
```

Expected: clean build for the binary crate.

---

## Task 6: Add `[exchange_margin]` defaults to TOML

**Files:**
- Modify: `config/default.toml` (top-level addition)
- Modify: `crates/integration-tests/fixtures/config_valid.toml` (mirror — see `crates/integration-tests/fixtures/`)

- [ ] **Step 6.1: Add to `config/default.toml`**

Add the following section near the existing `[pair_config.*]` blocks (e.g., right after the last `[pair_config.EUR_USD]` entry). Order within the file is irrelevant for TOML, but co-locating with related sections helps readers:

```toml
[exchange_margin.bitflyer_cfd]
# bitFlyer Crypto CFD: 維持率 50% 未満で即時ロスカット (公式)
# https://bitflyer.com/ja-jp/faq/7-23
liquidation_margin_level = 0.50

[exchange_margin.gmo_fx]
# GMOコイン外国為替FX: 維持率 100% 未満でロスカット (公式)
# https://support.coin.z.com/hc/ja/articles/17884183390105
liquidation_margin_level = 1.00
```

- [ ] **Step 6.2: Mirror into integration test fixture**

Read `crates/integration-tests/fixtures/config_valid.toml` and add the same two sections.

```bash
cat crates/integration-tests/fixtures/config_valid.toml | grep -A2 '^\[pair_config'
```

Add the two `[exchange_margin.*]` blocks above. Other fixtures (`config_missing_pairs.toml`, `config_missing_vegapunk.toml`, `config_invalid_strategy.toml`, `config_disabled_strategy.toml`) likely don't need them unless tests explicitly load active accounts — only update if tests fail in Task 7 because of missing entries.

---

## Task 7: Add fail-closed startup test

**Files:**
- Add: integration test verifying startup aborts when `[exchange_margin.<used_exchange>]` is missing.

- [ ] **Step 7.1: Decide test location**

Use `crates/integration-tests/tests/phase1_config.rs` (already covers config-loading paths). If this file doesn't have a startup harness that boots the full app against a DB, add the test in `crates/integration-tests/tests/phase1_startup.rs` instead.

```bash
grep -l 'spawn_test_app\|start_app\|run_main' crates/integration-tests/src/helpers/ crates/integration-tests/tests/ 2>/dev/null | head
```

Use whichever helper this exposes; if none exposes a "must-fail-on-startup" path, add one (`pub async fn try_spawn_app(config_path) -> Result<...>` returning the bind error rather than panicking).

- [ ] **Step 7.2: Write the failing test**

Add this test function (adapt path/helper names to whatever `spawn_test_app` your code provides):

```rust
#[tokio::test]
async fn startup_fails_when_exchange_margin_missing_for_active_account() {
    // Seed DB with a gmo_fx account, then load a config that omits
    // [exchange_margin.gmo_fx]. Startup must error out.
    let pool = crate::helpers::db::fresh_test_pool().await;
    crate::helpers::db::seed_account(
        &pool,
        "FX 安全",
        auto_trader_core::types::Exchange::GmoFx,
        "donchian_trend_v1",
        rust_decimal_macros::dec!(30000),
        rust_decimal_macros::dec!(10),
    )
    .await;

    let config_path = "crates/integration-tests/fixtures/config_missing_exchange_margin.toml";
    let result = crate::helpers::app::try_spawn_test_app(&pool, config_path).await;

    let err = result.expect_err("startup must fail when [exchange_margin.gmo_fx] missing");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("exchange_margin") && msg.contains("GmoFx"),
        "error must mention missing exchange_margin entry, got: {msg}"
    );
}
```

Create the missing fixture by copying `config_valid.toml` and removing both `[exchange_margin.*]` blocks:

```bash
cp crates/integration-tests/fixtures/config_valid.toml \
   crates/integration-tests/fixtures/config_missing_exchange_margin.toml
# Then edit the new file to delete the [exchange_margin.*] blocks.
```

- [ ] **Step 7.3: Run the new test**

```bash
docker compose up -d db
cargo test -p auto_trader_integration_tests startup_fails_when_exchange_margin_missing -- --nocapture
```

Expected: passes (Task 5's startup validation is what makes this pass).

---

## Task 8: Full test suite + commit

- [ ] **Step 8.1: Run unit tests**

```bash
cargo test -p auto_trader_core
cargo test -p auto_trader_executor
```

Expected: all green.

- [ ] **Step 8.2: Run integration tests**

```bash
docker compose up -d db
cargo test -p auto_trader_integration_tests
```

Expected: all green. If anything fails because a fixture lacks `[exchange_margin.*]`, add the sections to that fixture and re-run.

- [ ] **Step 8.3: Run lints**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Expected: no findings. If `cargo fmt --check` complains, run `cargo fmt --all` and re-run.

- [ ] **Step 8.4: Stage and commit Tasks 2–7 atomically**

```bash
git add crates/executor/src/position_sizer.rs \
        crates/executor/src/trader.rs \
        crates/executor/tests/trader_test.rs \
        crates/integration-tests/tests/phase3_close_flow.rs \
        crates/integration-tests/tests/phase3_execution.rs \
        crates/integration-tests/tests/phase3_execution_flow.rs \
        crates/integration-tests/tests/phase3_integrity.rs \
        crates/integration-tests/tests/phase3_monitoring.rs \
        crates/integration-tests/tests/phase1_config.rs \
        crates/integration-tests/fixtures/config_valid.toml \
        crates/integration-tests/fixtures/config_missing_exchange_margin.toml \
        crates/app/src/main.rs \
        config/default.toml

# Add db helper change if Task 5.2 introduced list_all_accounts
git add crates/db/src/trading_accounts.rs 2>/dev/null || true

git commit -m "$(cat <<'EOF'
feat(executor): broker-specific liquidation-aware position sizing

Replace hardcoded `maintenance_margin_rate=0.50` with per-exchange
`liquidation_margin_level` from `[exchange_margin.<name>]` config.
Formula: max_alloc = 1 / (Y + leverage × stop_loss_pct), so the SL
hit point coincides exactly with the broker's liquidation threshold.

- bitflyer_cfd: Y=0.50 (公式 50% 即時ロスカット)
- gmo_fx:      Y=1.00 (公式 100% 未満ロスカット)

Plumbed through Trader::new and resolved per-account at startup;
missing entries fail the process before any trading task spawns.

Spec: docs/superpowers/specs/2026-05-05-position-sizer-broker-liquidation-design.md

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 8.5: Push branch and open PR**

```bash
git push -u origin fix/position-sizer-broker-liquidation
gh pr create --title "fix(executor): broker-specific liquidation-aware position sizing" --body "$(cat <<'EOF'
## Summary
- Replace hardcoded `maintenance_margin_rate=0.50` in `PositionSizer` with per-exchange `liquidation_margin_level` from `[exchange_margin.<name>]` TOML config
- New formula `max_alloc = 1 / (Y + leverage × stop_loss_pct)` makes the SL hit point coincide with the broker's liquidation threshold
- Fail-closed startup: process aborts if any active account's exchange lacks an `[exchange_margin]` entry

Concrete impact (gmo_fx, lev=10, balance=30,000円):
- SL=0.5% → max_alloc 0.952 (= 28,560円 margin), SL hit時 維持率=100%
- SL=2.0% → max_alloc 0.833 (= 24,990円 margin), SL hit時 維持率=100%

bitflyer_cfd remains effectively unchanged (Y=0.50 + lev=2 yields max_alloc>1.0 → caps at allocation_pct=1.0).

Spec: `docs/superpowers/specs/2026-05-05-position-sizer-broker-liquidation-design.md`

## Test plan
- [ ] `cargo test -p auto_trader_core` passes
- [ ] `cargo test -p auto_trader_executor` passes
- [ ] `cargo test -p auto_trader_integration_tests` passes (DB up via docker compose)
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] Manual smoke test against a fresh DB: open a gmo_fx paper account, observe sizer leaves margin headroom (i.e., new trades' margin < balance, residual > 0 yen)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR opens. Capture URL. Do not merge.

---

## Self-Review

**Spec coverage check:**
- 数式 → Tasks 2.3, 2.4 (formula + tests)
- 設定 → Tasks 1, 6
- コンポーネント変更 (config.rs / position_sizer.rs / trader.rs / main.rs) → Tasks 1, 2, 3, 5
- 失敗モード (fail-closed startup) → Tasks 5, 7
- スコープ外項目 (open positions resize, slippage buffer, OANDA) → not implemented (correct)
- テスト計画 (unit 6+ / integration 3) → Tasks 2 (8 unit tests + invariant property), 7 (1 startup test); existing integration tests get signature updates

All spec sections covered.

**Placeholder scan:** No "TBD", "TODO" placeholders in tasks. Each step shows exact code or commands. Task 5.2 has a conditional fallback (if `list_accounts` is named differently) but provides the alternative concretely.

**Type consistency check:**
- `liquidation_margin_level: Decimal` consistent across `ExchangeMarginConfig.liquidation_margin_level`, `PositionSizer::calculate_quantity`, `Trader.liquidation_margin_level`, `Trader::new` arg.
- TOML key `exchange_margin` matches Rust field `exchange_margin` (serde default rename).
- `HashMap<String, ExchangeMarginConfig>` (raw TOML) → `HashMap<Exchange, Decimal>` (resolved at startup) — this is an intentional transition; the string-keyed form lets TOML parse, the Exchange-keyed form is what runtime callers use.
