# Vegapunk Evolution Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a regime-aware learning loop that ingests enriched trade data into Vegapunk, classifies market regimes, and auto-tunes the evolve strategy's parameters weekly using DB stats + Vegapunk search + LLM reasoning.

**Architecture:** Three layers: (1) data enrichment — every trade gets indicators + regime classification stored in both Postgres and Vegapunk; (2) evolve strategy — a parameterizable copy of donchian_trend that reads params from DB and validates signals via Vegapunk search + Wilson Score; (3) weekly batch — combines SQL aggregation, Vegapunk pattern search, and Gemini LLM to propose and apply parameter adjustments.

**Tech Stack:** Rust (sqlx, tonic/gRPC for Vegapunk, reqwest for Gemini), Postgres (3 schema changes), Vegapunk (fx-trading schema extension), Gemini API (flash model for weekly analysis). No frontend changes.

**Spec:** `docs/superpowers/specs/2026-04-10-vegapunk-evolution-design.md`

---

## File Structure

**New files:**
- `crates/market/src/indicators.rs` — add `adx` function (~40 lines)
- `crates/app/src/regime.rs` — `MarketRegime` enum + `classify_regime` function + tests
- `crates/app/src/wilson.rs` — `wilson_lower_bound` function + tests
- `crates/strategy/src/donchian_trend_evolve.rs` — parameterizable donchian (reads from DB, pre-trade Vegapunk search)
- `crates/app/src/weekly_batch.rs` — weekly analysis batch (DB stats + Vegapunk search + Gemini prompt + param update)
- `crates/app/src/enriched_ingest.rs` — trade ingest text generation with indicators + regime
- `migrations/20260410000001_evolve_strategy_and_params.sql` — strategy_params table, trades columns, account + catalog seed
- `schemas/fx-trading.yml` — extended with MarketRegime, Strategy param attrs, ParamChange, new edges

**Modified files:**
- `crates/market/src/bitflyer.rs` — track highs/lows alongside closes for ATR/ADX; add ATR, ADX, BB width to indicator map
- `crates/market/src/monitor.rs` — add ATR, ADX, BB width to OANDA indicator map (for future FX)
- `crates/strategy/src/lib.rs` — `pub mod donchian_trend_evolve;`
- `crates/app/src/main.rs` — register evolve strategy, enriched ingest calls, weekly batch task, daily Merge, feedback on close
- `config/default.toml` — evolve strategy entry

---

## Task 1: Migration + Vegapunk schema extension

**Files:**
- Create: `migrations/20260410000001_evolve_strategy_and_params.sql`
- Modify: `schemas/fx-trading.yml`

- [ ] **Step 1: Create the migration**

```sql
-- strategy_params: runtime-mutable parameters for evolve strategies.
-- Reads at startup + weekly batch update. Existing const-based
-- strategies never touch this table.
CREATE TABLE strategy_params (
    strategy_name TEXT PRIMARY KEY REFERENCES strategies(name),
    params JSONB NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- New columns on trades for enriched ingest + feedback tracking.
ALTER TABLE trades ADD COLUMN IF NOT EXISTS entry_indicators JSONB;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS vegapunk_search_id TEXT;

-- strategies catalog entry for the evolve variant.
INSERT INTO strategies (name, display_name, category, risk_level, description, algorithm, default_params)
VALUES ('donchian_trend_evolve_v1', 'ブレイクアウト進化版 (Donchian Evolve)',
        'crypto', 'medium',
        'donchian_trend_v1 ベース。Vegapunk 学習ループでパラメータを週次自動更新。baseline (normal) との比較検証用。',
        '(donchian_trend_v1 と同一アルゴリズム。パラメータのみ可変。)',
        '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb)
ON CONFLICT (name) DO NOTHING;

-- Initial params for evolve (identical to donchian_trend_v1 baseline).
INSERT INTO strategy_params (strategy_name, params)
VALUES ('donchian_trend_evolve_v1',
        '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb)
ON CONFLICT (strategy_name) DO NOTHING;

-- Evolve paper account.
INSERT INTO paper_accounts (id, name, exchange, initial_balance, current_balance,
                            currency, leverage, strategy, account_type, created_at, updated_at)
VALUES ('a0000000-0000-0000-0000-000000000020', 'crypto_evolve_v1',
        'bitflyer_cfd', 30000, 30000, 'JPY', 2,
        'donchian_trend_evolve_v1', 'paper', NOW(), NOW())
ON CONFLICT (id) DO NOTHING;
```

- [ ] **Step 2: Extend `schemas/fx-trading.yml`**

Read the current file, then append the new nodes/edges/traceable_pairs as specified in the spec (MarketRegime node, Strategy param attributes, ParamChange node, OCCURRED_IN/CHANGED_FROM/CHANGED_TO/MOTIVATED_BY edges, new traceable_pairs).

- [ ] **Step 3: Commit**

```bash
git add migrations/20260410000001_evolve_strategy_and_params.sql schemas/fx-trading.yml
git commit -m "feat(db): add strategy_params table, trades indicator columns, evolve account seed + schema extension"
```

---

## Task 2: ADX indicator + bitflyer candle tracking expansion

Bitflyer currently only tracks closes. ATR/ADX/BB need highs and lows.

**Files:**
- Modify: `crates/market/src/indicators.rs` — add `adx` function
- Modify: `crates/market/src/bitflyer.rs` — track highs/lows, add ATR/ADX/BB width to indicator map

- [ ] **Step 1: Implement ADX in `crates/market/src/indicators.rs`**

Append after the existing `atr` function:

```rust
/// Average Directional Index (ADX). Measures trend strength regardless
/// of direction. ADX > 25 suggests a trending market, < 20 suggests
/// range-bound. Requires `period + 1` bars minimum for the smoothed DX.
pub fn adx(
    highs: &[Decimal],
    lows: &[Decimal],
    closes: &[Decimal],
    period: usize,
) -> Option<Decimal> {
    if highs.len() < period * 2 + 1
        || lows.len() < period * 2 + 1
        || closes.len() < period * 2 + 1
    {
        return None;
    }
    let len = highs.len();
    let mut plus_dm_vals = Vec::with_capacity(len - 1);
    let mut minus_dm_vals = Vec::with_capacity(len - 1);
    let mut tr_vals = Vec::with_capacity(len - 1);
    for i in 1..len {
        let high_diff = highs[i] - highs[i - 1];
        let low_diff = lows[i - 1] - lows[i];
        let plus_dm = if high_diff > low_diff && high_diff > Decimal::ZERO {
            high_diff
        } else {
            Decimal::ZERO
        };
        let minus_dm = if low_diff > high_diff && low_diff > Decimal::ZERO {
            low_diff
        } else {
            Decimal::ZERO
        };
        let hl = highs[i] - lows[i];
        let hc = (highs[i] - closes[i - 1]).abs();
        let lc = (lows[i] - closes[i - 1]).abs();
        let tr = hl.max(hc).max(lc);
        plus_dm_vals.push(plus_dm);
        minus_dm_vals.push(minus_dm);
        tr_vals.push(tr);
    }
    if plus_dm_vals.len() < period * 2 {
        return None;
    }
    // Wilder's smoothing for +DM, -DM, TR
    let p = Decimal::from(period as i64);
    let mut smooth_plus: Decimal = plus_dm_vals[..period].iter().sum();
    let mut smooth_minus: Decimal = minus_dm_vals[..period].iter().sum();
    let mut smooth_tr: Decimal = tr_vals[..period].iter().sum();

    let mut dx_vals = Vec::new();
    for i in period..plus_dm_vals.len() {
        smooth_plus = smooth_plus - smooth_plus / p + plus_dm_vals[i];
        smooth_minus = smooth_minus - smooth_minus / p + minus_dm_vals[i];
        smooth_tr = smooth_tr - smooth_tr / p + tr_vals[i];
        if smooth_tr == Decimal::ZERO {
            continue;
        }
        let plus_di = smooth_plus / smooth_tr * Decimal::from(100);
        let minus_di = smooth_minus / smooth_tr * Decimal::from(100);
        let di_sum = plus_di + minus_di;
        if di_sum == Decimal::ZERO {
            dx_vals.push(Decimal::ZERO);
        } else {
            let dx = ((plus_di - minus_di).abs() / di_sum) * Decimal::from(100);
            dx_vals.push(dx);
        }
    }
    if dx_vals.len() < period {
        return None;
    }
    // First ADX = SMA of first `period` DX values
    let first_adx: Decimal =
        dx_vals[..period].iter().sum::<Decimal>() / p;
    // Smooth subsequent ADX values
    let mut adx_val = first_adx;
    for dx in &dx_vals[period..] {
        adx_val = (adx_val * (p - Decimal::ONE) + dx) / p;
    }
    Some(adx_val)
}
```

- [ ] **Step 2: Expand bitflyer candle tracking to include highs/lows**

In `crates/market/src/bitflyer.rs`, the existing code tracks only closes:

```rust
let closes = closes_map.entry(product_code.clone()).or_default();
closes.push(candle.close);
if closes.len() > 200 {
    closes.drain(..closes.len() - 200);
}
```

This needs to be expanded. Change the `closes_map` type from `HashMap<String, Vec<Decimal>>` to a struct that carries highs, lows, and closes. The simplest approach: add two parallel maps `highs_map` and `lows_map` next to `closes_map`, or create a `CandleHistory` struct. Use whichever is least invasive.

Then add ATR, ADX, and BB width percentage to the indicator map:

```rust
// After the existing SMA/RSI block:
let highs = highs_map.entry(product_code.clone()).or_default();
let lows = lows_map.entry(product_code.clone()).or_default();
highs.push(candle.high);
lows.push(candle.low);
if highs.len() > 200 { highs.drain(..highs.len() - 200); }
if lows.len() > 200 { lows.drain(..lows.len() - 200); }

if let Some(v) = indicators::atr(highs, lows, closes, 14) {
    indicator_map.insert("atr_14".to_string(), v);
}
if let Some(v) = indicators::adx(highs, lows, closes, 14) {
    indicator_map.insert("adx_14".to_string(), v);
}
// BB width as percentage of SMA20
if let Some((bb_lo, bb_mid, bb_up)) = indicators::bollinger_bands(closes, 20, dec!(2)) {
    if bb_mid > Decimal::ZERO {
        let bb_width_pct = (bb_up - bb_lo) / bb_mid * dec!(100);
        indicator_map.insert("bb_width_pct".to_string(), bb_width_pct);
    }
}
// ATR percentile (rank within last 50 ATR values)
// Compute inline: count how many of the last 50 ATR values are below the current one
if let Some(current_atr) = indicator_map.get("atr_14").copied() {
    let lookback = 50.min(closes.len());
    if lookback >= 14 {
        let mut atr_count_below = 0u32;
        let mut atr_total = 0u32;
        for end in (closes.len() - lookback)..closes.len() {
            if end >= 14 {
                if let Some(past_atr) = indicators::atr(
                    &highs[..=end], &lows[..=end], &closes[..=end], 14
                ) {
                    atr_total += 1;
                    if past_atr < current_atr { atr_count_below += 1; }
                }
            }
        }
        if atr_total > 0 {
            let pct = Decimal::from(atr_count_below) / Decimal::from(atr_total) * dec!(100);
            indicator_map.insert("atr_percentile".to_string(), pct);
        }
    }
}
```

Also update the warmup seed type and `with_closes_seed` builder to carry highs/lows (or refactor the seed to use full `Candle` structs from DB warmup).

- [ ] **Step 3: Add the same indicators to `crates/market/src/monitor.rs`**

The OANDA monitor already has highs/lows available from candles. Add ATR, ADX, BB width, and ATR percentile to its indicator block following the same pattern.

- [ ] **Step 4: cargo check + clippy + test**

```bash
source ~/.cargo/env
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 5: Commit**

```bash
git add crates/market/src/indicators.rs crates/market/src/bitflyer.rs crates/market/src/monitor.rs
git commit -m "feat(market): add ADX indicator, expand bitflyer to track highs/lows, enrich indicator map"
```

---

## Task 3: Regime classification + Wilson Score

Pure functions with tests. No external dependencies.

**Files:**
- Create: `crates/app/src/regime.rs`
- Create: `crates/app/src/wilson.rs`
- Modify: `crates/app/src/main.rs` — add `mod regime; mod wilson;`

- [ ] **Step 1: Create `crates/app/src/regime.rs`**

```rust
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketRegime {
    Trend,
    Range,
    HighVol,
    EventWindow,
}

impl MarketRegime {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Trend => "trend",
            Self::Range => "range",
            Self::HighVol => "high_vol",
            Self::EventWindow => "event_window",
        }
    }
}

/// Classify the current market regime from indicator values.
/// EventWindow requires external data (news calendar) and is not
/// auto-detected — returns None if indicators are insufficient.
pub fn classify(indicators: &HashMap<String, Decimal>) -> MarketRegime {
    let adx = indicators.get("adx_14").copied();
    let atr_pct = indicators.get("atr_percentile").copied();

    // High volatility takes priority (dangerous regardless of trend)
    if let Some(pct) = atr_pct {
        if pct > dec!(80) {
            return MarketRegime::HighVol;
        }
    }
    // ADX > 25 = trending market
    if let Some(adx_val) = adx {
        if adx_val > dec!(25) {
            return MarketRegime::Trend;
        }
    }
    // Default: range-bound
    MarketRegime::Range
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indicators(adx: f64, atr_pct: f64) -> HashMap<String, Decimal> {
        let mut m = HashMap::new();
        m.insert("adx_14".to_string(), Decimal::try_from(adx).unwrap());
        m.insert("atr_percentile".to_string(), Decimal::try_from(atr_pct).unwrap());
        m
    }

    #[test]
    fn high_vol_takes_priority() {
        // ADX is high (trending) but ATR percentile > 80 → high_vol wins
        assert_eq!(classify(&indicators(30.0, 85.0)), MarketRegime::HighVol);
    }

    #[test]
    fn trend_when_adx_above_25() {
        assert_eq!(classify(&indicators(30.0, 50.0)), MarketRegime::Trend);
    }

    #[test]
    fn range_when_adx_below_25() {
        assert_eq!(classify(&indicators(20.0, 50.0)), MarketRegime::Range);
    }

    #[test]
    fn range_when_no_indicators() {
        assert_eq!(classify(&HashMap::new()), MarketRegime::Range);
    }
}
```

- [ ] **Step 2: Create `crates/app/src/wilson.rs`**

```rust
/// Wilson Score lower bound for a binomial proportion.
/// Gives a conservative estimate of the true win rate given
/// observed wins/total and a confidence level.
///
/// z = 1.96 for 95% confidence (default).
pub fn lower_bound(wins: u64, total: u64, z: f64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let n = total as f64;
    let p = wins as f64 / n;
    let z2 = z * z;
    let numerator = p + z2 / (2.0 * n)
        - z * ((p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt());
    let denominator = 1.0 + z2 / n;
    (numerator / denominator).max(0.0)
}

/// Convenience: 95% confidence Wilson lower bound.
pub fn lower_bound_95(wins: u64, total: u64) -> f64 {
    lower_bound(wins, total, 1.96)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_total_returns_zero() {
        assert_eq!(lower_bound_95(0, 0), 0.0);
    }

    #[test]
    fn all_wins_small_sample() {
        // 3 wins out of 3 → lower bound should still be < 1.0
        let lb = lower_bound_95(3, 3);
        assert!(lb > 0.3);
        assert!(lb < 1.0);
    }

    #[test]
    fn no_wins_returns_near_zero() {
        let lb = lower_bound_95(0, 10);
        assert!(lb < 0.05);
    }

    #[test]
    fn fifty_percent_moderate_sample() {
        // 10 wins out of 20 → lb should be meaningfully below 0.5
        let lb = lower_bound_95(10, 20);
        assert!(lb > 0.25);
        assert!(lb < 0.50);
    }

    #[test]
    fn large_sample_converges() {
        // 700 wins out of 1000 → lb should be close to 0.70
        let lb = lower_bound_95(700, 1000);
        assert!(lb > 0.66);
        assert!(lb < 0.70);
    }
}
```

- [ ] **Step 3: Declare modules in `main.rs`**

Add near the top of `crates/app/src/main.rs`:

```rust
mod regime;
mod wilson;
```

- [ ] **Step 4: cargo check + test**

```bash
source ~/.cargo/env
cargo check -p auto-trader
cargo test -p auto-trader regime::tests wilson::tests
```

Expected: 9 tests passing (4 regime + 5 wilson).

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/regime.rs crates/app/src/wilson.rs crates/app/src/main.rs
git commit -m "feat(app): add MarketRegime classifier + Wilson Score lower bound"
```

---

## Task 4: Enriched ingest module

Extract the trade-ingest text generation into its own module for testability and to keep `main.rs` from growing further.

**Files:**
- Create: `crates/app/src/enriched_ingest.rs`
- Modify: `crates/app/src/main.rs` — add `mod enriched_ingest;`

- [ ] **Step 1: Create `crates/app/src/enriched_ingest.rs`**

Two public functions: `format_trade_open` and `format_trade_close`. They take trade data + indicators and return the enriched text string for Vegapunk ingest.

```rust
use auto_trader_core::types::{Direction, ExitReason, Trade};
use crate::regime::{self, MarketRegime};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Format an enriched ingest text for trade OPEN events.
/// Includes indicators, regime classification, and SMA deviation.
pub fn format_trade_open(
    trade: &Trade,
    indicators: &HashMap<String, Decimal>,
) -> String {
    let dir = match trade.direction {
        Direction::Long => "ロング",
        Direction::Short => "ショート",
    };
    let regime = regime::classify(indicators);
    let sma20_dev = indicators.get("sma_20").and_then(|sma| {
        if *sma > Decimal::ZERO {
            Some(((trade.entry_price - sma) / sma * Decimal::from(100)).round_dp(2))
        } else {
            None
        }
    });

    let mut text = format!(
        "[{}] {} {} エントリー。\n\
         ▸ 戦略: {} (allocation: {}%)\n\
         ▸ 価格: {} / SL: {} / TP: {}\n\
         ▸ 数量: {}\n\
         ▸ レジーム: {}\n\
         ▸ 指標:",
        trade.exchange,
        trade.pair,
        dir,
        trade.strategy_name,
        trade.pnl_amount.map(|_| "N/A").unwrap_or("100"), // allocation is on Signal, not Trade — use a reasonable default
        trade.entry_price,
        trade.stop_loss,
        trade.take_profit,
        trade.quantity.map(|q| q.to_string()).unwrap_or_else(|| "-".to_string()),
        regime.as_str(),
    );

    for (key, val) in indicators.iter() {
        text.push_str(&format!(" {}={},", key, val.round_dp(2)));
    }
    if let Some(dev) = sma20_dev {
        text.push_str(&format!("\n▸ SMA20乖離: {}%", dev));
    }
    text
}

/// Format an enriched ingest text for trade CLOSE events.
/// Includes outcome, holding time, entry indicators from DB, and
/// a rule-based post-mortem hint.
pub fn format_trade_close(
    trade: &Trade,
    entry_indicators: Option<&serde_json::Value>,
    account_balance: Option<Decimal>,
    account_initial: Option<Decimal>,
) -> String {
    let dir = match trade.direction {
        Direction::Long => "ロング",
        Direction::Short => "ショート",
    };
    let pnl = trade.pnl_amount.unwrap_or_default();
    let fees = trade.fees;
    let net_pnl = pnl - fees;
    let exit_reason = trade.exit_reason
        .map(|r| format!("{:?}", r))
        .unwrap_or_else(|| "unknown".to_string());

    let holding = trade.exit_at.map(|exit| {
        let dur = exit.signed_duration_since(trade.entry_at);
        let mins = dur.num_minutes();
        if mins < 60 { format!("{}分", mins) }
        else { format!("{}時間{}分", mins / 60, mins % 60) }
    }).unwrap_or_else(|| "-".to_string());

    let price_change_pct = trade.exit_price.map(|exit| {
        if trade.entry_price > Decimal::ZERO {
            ((exit - trade.entry_price) / trade.entry_price * Decimal::from(100)).round_dp(2)
        } else {
            Decimal::ZERO
        }
    });

    let mut text = format!(
        "[{}] {} {} 決済。\n\
         ▸ 戦略: {}\n\
         ▸ 結果: {} / PnL: {} / 手数料: {} / 純損益: {}\n\
         ▸ 保有時間: {}\n\
         ▸ エントリー: {} → 決済: {}",
        trade.exchange,
        trade.pair,
        dir,
        trade.strategy_name,
        exit_reason,
        pnl, fees, net_pnl,
        holding,
        trade.entry_price,
        trade.exit_price.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string()),
    );

    if let Some(pct) = price_change_pct {
        text.push_str(&format!(" (変動率: {}%)", pct));
    }

    // Account balance context
    if let (Some(bal), Some(init)) = (account_balance, account_initial) {
        if init > Decimal::ZERO {
            let bal_pct = ((bal - init) / init * Decimal::from(100)).round_dp(1);
            text.push_str(&format!(
                "\n▸ 口座残高: {} (初期比: {}%)", bal, bal_pct
            ));
        }
    }

    // Entry indicators from JSONB
    if let Some(ind) = entry_indicators {
        if let Some(regime) = ind.get("regime").and_then(|v| v.as_str()) {
            text.push_str(&format!("\n▸ エントリー時レジーム: {}", regime));
        }
        if let Some(rsi) = ind.get("rsi_14") {
            text.push_str(&format!(", RSI: {}", rsi));
        }
        if let Some(atr) = ind.get("atr_14") {
            text.push_str(&format!(", ATR: {}", atr));
        }
        if let Some(adx) = ind.get("adx_14") {
            text.push_str(&format!(", ADX: {}", adx));
        }
    }

    // Rule-based post-mortem
    text.push_str(&format!("\n▸ 反省材料: {}", post_mortem(trade, entry_indicators)));

    text
}

fn post_mortem(trade: &Trade, entry_indicators: Option<&serde_json::Value>) -> &'static str {
    let is_loss = trade.pnl_amount.map(|p| p < Decimal::ZERO).unwrap_or(false);
    let is_sl = trade.exit_reason == Some(ExitReason::SlHit);
    let rsi_high = entry_indicators
        .and_then(|i| i.get("rsi_14"))
        .and_then(|v| v.as_str().or_else(|| v.as_f64().map(|_| "")))
        .and_then(|s| s.parse::<f64>().ok())
        .map(|r| r > 65.0)
        .unwrap_or(false);
    let rsi_low = entry_indicators
        .and_then(|i| i.get("rsi_14"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .map(|r| r < 35.0)
        .unwrap_or(false);

    if is_sl && rsi_high && trade.direction == Direction::Long {
        "RSI 過熱圏でのロング、逆行リスクが高い局面"
    } else if is_sl && rsi_low && trade.direction == Direction::Short {
        "RSI 売られすぎ圏でのショート、反発リスクが高い局面"
    } else if is_sl && is_loss {
        "損切り発動、SL 距離の見直しまたはエントリー条件の精査が必要"
    } else if trade.exit_reason == Some(ExitReason::StrategyTimeLimit) {
        "時間切れ。トレンド未発生、エントリー条件の見直し候補"
    } else if !is_loss {
        "想定通りの利益確定。条件の再現性あり"
    } else {
        "分析データ不足"
    }
}
```

- [ ] **Step 2: Declare module in main.rs**

Add `mod enriched_ingest;` near the other module declarations.

- [ ] **Step 3: cargo check**

```bash
source ~/.cargo/env
cargo check -p auto-trader
```

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/enriched_ingest.rs crates/app/src/main.rs
git commit -m "feat(app): enriched ingest text formatter with regime + indicators + post-mortem"
```

---

## Task 5: `donchian_trend_evolve_v1` strategy

Copy of `donchian_trend_v1` that reads params from DB and optionally validates signals via Vegapunk search.

**Files:**
- Create: `crates/strategy/src/donchian_trend_evolve.rs`
- Modify: `crates/strategy/src/lib.rs`

This task follows the same pattern as the existing `donchian_trend.rs` but:
1. `new()` takes a `params: serde_json::Value` containing all tunable constants
2. On `on_price`, after generating a Signal, optionally performs a Vegapunk pre-trade validation (if a `vegapunk_client` is available)
3. Reads params from the JSON at construction time, falling back to defaults

The strategy struct holds parsed params as fields:

```rust
pub struct DonchianTrendEvolveV1 {
    name: String,
    pairs: Vec<Pair>,
    entry_channel: usize,
    exit_channel: usize,
    sl_pct: Decimal,
    allocation_pct: Decimal,
    atr_baseline_bars: usize,
    history: HashMap<String, VecDeque<Candle>>,
}

impl DonchianTrendEvolveV1 {
    pub fn new(name: String, pairs: Vec<Pair>, params: serde_json::Value) -> Self {
        Self {
            name,
            pairs,
            entry_channel: params["entry_channel"].as_u64().unwrap_or(20) as usize,
            exit_channel: params["exit_channel"].as_u64().unwrap_or(10) as usize,
            sl_pct: /* parse from params or default 0.03 */,
            allocation_pct: /* parse from params or default 1.0 */,
            atr_baseline_bars: params["atr_baseline_bars"].as_u64().unwrap_or(50) as usize,
            history: HashMap::new(),
        }
    }
}
```

The `on_price` / `on_open_positions` / `warmup` implementations mirror `donchian_trend_v1` exactly, but use `self.entry_channel` etc. instead of `const ENTRY_CHANNEL`. Filter for `Exchange::BitflyerCfd` (same as baseline).

Unit tests: constructor parses JSON correctly, defaults apply when keys are missing.

- [ ] **Step 1: Create the strategy file**

Full implementation mirroring `donchian_trend.rs` with parameterized fields.

- [ ] **Step 2: Add to `crates/strategy/src/lib.rs`**

```rust
pub mod donchian_trend_evolve;
```

- [ ] **Step 3: cargo check + test**

- [ ] **Step 4: Commit**

```bash
git add crates/strategy/src/donchian_trend_evolve.rs crates/strategy/src/lib.rs
git commit -m "feat(strategy): donchian_trend_evolve_v1 with DB-parameterized constants"
```

---

## Task 6: Weekly batch — DB stats + Vegapunk search + Gemini → param update

The core intelligence loop.

**Files:**
- Create: `crates/app/src/weekly_batch.rs`

- [ ] **Step 1: Create `crates/app/src/weekly_batch.rs`**

```rust
use auto_trader_vegapunk::client::VegapunkClient;
use chrono::{Datelike, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Run the weekly evolution batch. Called from daily batch when
/// day-of-week == Sunday (JST).
pub async fn run(
    pool: &PgPool,
    vegapunk: Option<&Arc<Mutex<VegapunkClient>>>,
    gemini_api_url: &str,
    gemini_api_key: &str,
    gemini_model: &str,
    vegapunk_schema: &str,
) -> anyhow::Result<()> {
    tracing::info!("weekly evolution batch: starting");

    // Step 1: DB stats for the past week
    let stats = fetch_weekly_stats(pool).await?;
    tracing::info!("weekly stats: {} trades across all strategies", stats.total_trades);

    if stats.total_trades < 5 {
        tracing::info!("weekly evolution batch: insufficient trades ({}), skipping", stats.total_trades);
        return Ok(());
    }

    // Step 2: Vegapunk search for patterns
    let vp_context = if let Some(vp) = vegapunk {
        let mut client = vp.lock().await;
        let search_result = client.search(
            "donchian_trend の損失パターン、レジーム別の傾向、直近のパラメータ変更結果",
            "global",
            10,
        ).await;
        match search_result {
            Ok(resp) => {
                // Return feedback on this search after we see results
                let search_id = resp.search_id.clone();
                let context_text = resp.results.iter()
                    .filter_map(|r| r.text.as_ref())
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n---\n");
                Some((search_id, context_text))
            }
            Err(e) => {
                tracing::warn!("weekly batch: vegapunk search failed: {e}");
                None
            }
        }
    } else {
        None
    };

    // Step 3: Get current evolve params
    let current_params: serde_json::Value = sqlx::query_scalar(
        "SELECT params FROM strategy_params WHERE strategy_name = 'donchian_trend_evolve_v1'"
    )
    .fetch_optional(pool)
    .await?
    .unwrap_or_else(|| serde_json::json!({
        "entry_channel": 20, "exit_channel": 10,
        "sl_pct": 0.03, "allocation_pct": 1.0, "atr_baseline_bars": 50
    }));

    // Step 4: Wilson Score analysis per regime
    let wilson_analysis = compute_regime_wilson(pool).await?;

    // Step 5: Build LLM prompt and call Gemini
    let prompt = build_gemini_prompt(&stats, &vp_context, &current_params, &wilson_analysis);
    let proposed = call_gemini(gemini_api_url, gemini_api_key, gemini_model, &prompt).await?;

    tracing::info!("weekly evolution batch: proposed params = {}", proposed.params);
    tracing::info!("weekly evolution batch: rationale = {}", proposed.rationale);

    // Step 6: Update strategy_params in DB
    sqlx::query(
        "UPDATE strategy_params SET params = $1, updated_at = NOW() WHERE strategy_name = 'donchian_trend_evolve_v1'"
    )
    .bind(&proposed.params)
    .execute(pool)
    .await?;

    // Step 7: Ingest param_change to Vegapunk
    if let Some(vp) = vegapunk {
        let mut client = vp.lock().await;
        let week = Utc::now().iso_week().week();
        let text = format!(
            "donchian_trend_evolve_v1 パラメータ更新 (2026-W{})。\n\
             ▸ 変更前: {}\n\
             ▸ 変更後: {}\n\
             ▸ 根拠: {}\n\
             ▸ 期待効果: {}",
            week, current_params, proposed.params, proposed.rationale, proposed.expected_effect
        );
        if let Err(e) = client.ingest_raw(
            &text, "param_change", "strategy-evolution",
            &Utc::now().to_rfc3339(),
        ).await {
            tracing::warn!("weekly batch: vegapunk param_change ingest failed: {e}");
        }
    }

    // Step 8: Create notification
    let notif_text = format!(
        "donchian_trend_evolve_v1 パラメータ自動更新。根拠: {}",
        proposed.rationale
    );
    // Insert into notifications table (reuse existing notifications module)
    auto_trader_db::notifications::insert_system_notification(pool, &notif_text).await
        .unwrap_or_else(|e| tracing::warn!("weekly batch: notification insert failed: {e}"));

    tracing::info!("weekly evolution batch: completed");
    Ok(())
}

// ... internal helper functions: fetch_weekly_stats, compute_regime_wilson,
//     build_gemini_prompt, call_gemini, GeminiProposal struct
```

The implementation includes:
- `fetch_weekly_stats` — SQL aggregation of trades for the past week
- `compute_regime_wilson` — group trades by `entry_indicators->'regime'`, compute Wilson lower bound per group
- `build_gemini_prompt` — assemble the structured prompt from spec
- `call_gemini` — HTTP POST to Gemini API, parse JSON response
- `GeminiProposal` — struct for the parsed response

- [ ] **Step 2: Add `mod weekly_batch;` to `main.rs`**

- [ ] **Step 3: cargo check**

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/weekly_batch.rs crates/app/src/main.rs
git commit -m "feat(app): weekly evolution batch (DB stats + Vegapunk search + Gemini param proposal)"
```

---

## Task 7: main.rs wiring — strategy registration, ingest, feedback, weekly/merge scheduling

Wire everything together in `main.rs`.

**Files:**
- Modify: `crates/app/src/main.rs`
- Modify: `config/default.toml`

- [ ] **Step 1: Register evolve strategy**

In the strategy match block, add before `donchian_trend`:

```rust
name if name.starts_with("donchian_trend_evolve") => {
    let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
    // Read params from DB at startup (strategy_params table)
    let params: serde_json::Value = sqlx::query_scalar(
        "SELECT params FROM strategy_params WHERE strategy_name = $1"
    )
    .bind(&sc.name)
    .fetch_optional(&pool)
    .await
    .unwrap_or(None)
    .unwrap_or_else(|| serde_json::json!({
        "entry_channel": 20, "exit_channel": 10,
        "sl_pct": 0.03, "allocation_pct": 1.0, "atr_baseline_bars": 50
    }));
    engine.add_strategy(
        Box::new(auto_trader_strategy::donchian_trend_evolve::DonchianTrendEvolveV1::new(
            sc.name.clone(), pairs, params,
        )),
        sc.mode.clone(),
    );
    tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
}
```

- [ ] **Step 2: Enrich the trade OPEN ingest**

In the signal execution block where `vp.ingest_raw` is called for trade open, replace the existing text formatting with:

```rust
let text = crate::enriched_ingest::format_trade_open(&trade, &event.indicators);
```

Also save `entry_indicators` to the trades table:

```rust
let indicators_json = serde_json::to_value(&event.indicators).unwrap_or_default();
// Add regime classification
let mut ind_with_regime = indicators_json.as_object().cloned().unwrap_or_default();
ind_with_regime.insert("regime".to_string(),
    serde_json::Value::String(crate::regime::classify(&event.indicators).as_str().to_string()));
sqlx::query("UPDATE trades SET entry_indicators = $1 WHERE id = $2")
    .bind(serde_json::Value::Object(ind_with_regime))
    .bind(trade.id)
    .execute(&pool)
    .await
    .unwrap_or_else(|e| { tracing::warn!("failed to save entry_indicators: {e}"); Default::default() });
```

- [ ] **Step 3: Enrich the trade CLOSE ingest**

In the recorder task where `vp.ingest_raw` is called for trade close, replace existing text with:

```rust
// Fetch entry_indicators and account info from DB
let entry_ind: Option<serde_json::Value> = sqlx::query_scalar(
    "SELECT entry_indicators FROM trades WHERE id = $1"
).bind(t.id).fetch_optional(&recorder_pool).await.unwrap_or(None);

let (bal, init): (Option<Decimal>, Option<Decimal>) = match t.paper_account_id {
    Some(aid) => sqlx::query_as(
        "SELECT current_balance, initial_balance FROM paper_accounts WHERE id = $1"
    ).bind(aid).fetch_optional(&recorder_pool).await.unwrap_or(None).unwrap_or((None, None)),
    None => (None, None),
};

let text = crate::enriched_ingest::format_trade_close(&t, entry_ind.as_ref(), bal, init);
```

- [ ] **Step 4: Auto feedback on trade close**

After the ingest in the recorder task, if the trade has a `vegapunk_search_id`:

```rust
if let Some(search_id) = &t.vegapunk_search_id {
    // ... (read from a new column, or from entry_indicators JSON)
    let rating = crate::enriched_ingest::compute_feedback_rating(&t);
    let comment = format!("PnL: {}, regime: {}", net_pnl,
        entry_ind.as_ref().and_then(|i| i.get("regime")).and_then(|v| v.as_str()).unwrap_or("unknown"));
    if let Some(vp) = vegapunk_client_recorder.clone() {
        let search_id = search_id.clone();
        tokio::spawn(async move {
            let mut vp = vp.lock().await;
            if let Err(e) = vp.feedback(&search_id, rating, &comment).await {
                tracing::warn!("vegapunk feedback failed: {e}");
            }
        });
    }
}
```

- [ ] **Step 5: Weekly batch + daily Merge scheduling**

In the daily batch task (around the `if now_date != last_date { ... }` block), add:

```rust
// Daily: run Vegapunk Merge for community detection
if let Some(vp) = vegapunk_client_daily.clone() {
    tokio::spawn(async move {
        let mut client = vp.lock().await;
        if let Err(e) = client.merge().await {
            tracing::warn!("daily vegapunk merge failed: {e}");
        } else {
            tracing::info!("daily vegapunk merge completed");
        }
    });
}

// Weekly (Sunday JST): run evolution batch
let jst_weekday = (chrono::Utc::now() + chrono::Duration::hours(9)).weekday();
if jst_weekday == chrono::Weekday::Sun {
    if let Err(e) = crate::weekly_batch::run(
        &daily_pool,
        vegapunk_client_weekly.as_ref(),
        &gemini_api_url, &gemini_api_key, &gemini_model,
        &config.vegapunk.schema,
    ).await {
        tracing::error!("weekly evolution batch failed: {e}");
    }
}
```

- [ ] **Step 6: Add evolve strategy to `config/default.toml`**

```toml
[[strategies]]
name = "donchian_trend_evolve_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY"]
# Evolve: Vegapunk 学習ループで週次パラメータ自動更新。
# 実行時パラメータは DB (strategy_params テーブル) から読む。
# ここの params は初回 seed 時のデフォルト参照用。
params = { entry_channel = 20, exit_channel = 10, sl_pct = 0.03, allocation_pct = 1.00, atr_baseline_bars = 50 }
```

- [ ] **Step 7: Full verification**

```bash
source ~/.cargo/env
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 8: Commit**

```bash
git add crates/app/src/main.rs config/default.toml
git commit -m "feat(app): wire evolve strategy, enriched ingest, feedback, weekly batch + daily merge"
```

---

## Task 8: Integration verification

**Files:** None (verification only)

- [ ] **Step 1: Docker rebuild**

```bash
docker compose build auto-trader
docker compose up -d auto-trader
sleep 10
docker logs auto-trader-auto-trader-1 --tail 40
```

Expected:
- `strategy registered: donchian_trend_evolve_v1 (mode=paper)`
- No panics
- `vegapunk connected` or `vegapunk unavailable` (both OK for now)

- [ ] **Step 2: DB verification**

```bash
docker exec auto-trader-db-1 psql -U auto-trader -d auto_trader -c "SELECT name, strategy FROM paper_accounts WHERE name LIKE '%evolve%';"
docker exec auto-trader-db-1 psql -U auto-trader -d auto_trader -c "SELECT * FROM strategy_params;"
docker exec auto-trader-db-1 psql -U auto-trader -d auto_trader -c "SELECT column_name FROM information_schema.columns WHERE table_name='trades' AND column_name IN ('entry_indicators','vegapunk_search_id');"
```

- [ ] **Step 3: Indicator verification**

Wait for a few candle ticks, then check:

```bash
curl -s http://localhost:3001/api/market/prices | python3 -m json.tool
```

Also check logs for any indicator computation errors.

- [ ] **Step 4: Report**

---

## Post-Implementation

1. Run `code-review` skill flow.
2. Push + PR.
3. Monitor for first evolve trade (may take hours/days depending on market).
4. Next Sunday: first weekly batch runs automatically.
5. **Separate PR:** squeeze_momentum frequency improvement.
