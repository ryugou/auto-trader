# GMO FX Swap Point (paper accrual) Design

## Goal

GMO FX **paper account** でも config 固定 rate ベースで daily swap point を `Trade.fees` に積算し、bitFlyer SFD/overnight_fee と並ぶ「paper=live contract」を成立させる。live は GMO 取引所が balance に自動反映するため bot 側で `Trade.fees` に反映しない (PR A の paper liquidation と同じ「live=exchange 任せ、paper=bot 代行」パターン)。

## Context

- GMO FX swap point は通貨ペア × 方向 (Long 受取/Short 支払い、逆も有り) で発生し、毎営業日 NY close で計上される。
- 既存 `GmoOpenPosition` (`crates/market/src/gmo_fx_private.rs:130`) に **per-position swap field 無し**。`GmoAccountAssets.total_swap` は account 集計値のみで position に attribute できない。よって live API ベース実装は分離困難 → paper 専用設計を採用。
- bitFlyer SFD (PR A、PR #92 merged) と同じく `apply_*_fee` + `account_events` パターン。今回は新 helper `apply_swap_fee` を追加。
- 既存 `overnight_handle` (`crates/app/src/main.rs:1719-1822`) は bitFlyer のみ daily で fee を引く。GMO FX 経路を同 task 内に追加して 1 つの cron に統合する。

## Non-Goals

- live GMO API から swap 実値を取得する path (上記理由で困難 + spec 上 paper 専用判断)。
- 3 倍デー (NY 火曜→水曜の roll で 3 日分計上、土日対策) — 最初は 1 倍デーのみ、将来 config 拡張で対応。
- swap rate の動的取得 (GMO 公式 page scrape, 動的 API): YAGNI。代表値 hardcode で paper 近似十分。
- PR A で deferred の **restart 跨ぎ persistence**: schema 変更を伴うため別 follow-up PR。
  - 注: 元 spec では「apply_*_fee 共通化 refactor も別 PR」と記載していたが、本 PR の simplify
    review で N=3 重複が defer 不能と判断、`apply_fee_inner` を private fn として **本 PR で
    抽出済** (Reuse W1 対応)。

## Architecture

### 1. Config (新規セクション)

`crates/core/src/config.rs::AppConfig` に追加:

```rust
/// Per-pair × per-direction swap rates for GMO FX (in JPY per lot per day).
/// signed: **positive = paper account pays, negative = receives**
/// (apply_swap_fee の符号規約と一致)。
/// 1 lot = 10,000 通貨単位 (GMO FX 標準)。
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

`AppConfig` に `pub gmo_fx: GmoFxConfig` を追加 (TOML key `[gmo_fx]`、その下に `swap: GmoFxSwapConfig`):

```toml
[gmo_fx.swap.rates]
USD_JPY = { long = 100, short = -120 }
EUR_JPY = { long = 80, short = -100 }
# 未登録 pair は 0 扱い (該当 trade を skip)
```

### 2. Pure 関数 (`crates/core/src/swap.rs` 新規)

`commission.rs` / `sfd.rs` と同形:

```rust
use crate::types::{Direction, Exchange};
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

/// paper 側 swap fee の estimate skeleton。現状は全 exchange 0 を返す。
/// 実際の swap 計算は `compute_daily_swap` が config rate を使って行う。
pub fn estimate(_exchange: Exchange) -> Decimal {
    Decimal::ZERO
}

/// 1 日分の swap fee (signed) を算出。
///
/// formula:
///   per_lot = rate.long or rate.short (direction で分岐)
///   lots    = quantity / GMO_FX_LOT_SIZE (10_000)
///   fee     = truncate_yen(per_lot × lots)
///
/// 戻り値が正なら paper account は **払い** (fees 増・balance 減)、負なら
/// **受取** (fees 減・balance 増)。`apply_swap_fee` の符号規約と一致。
pub fn compute_daily_swap(
    rate: SwapRateEntry,
    direction: Direction,
    quantity: Decimal,
) -> Decimal {
    let per_lot = match direction {
        Direction::Long => rate.long,
        Direction::Short => rate.short,
    };
    let lots = quantity / dec!(10_000);
    (per_lot * lots).round_dp_with_strategy(0, RoundingStrategy::ToZero)
}
```

### 3. DB helper (`crates/db/src/trades.rs`)

`apply_sfd_fee` の clone (event_type 違いのみ):

```rust
pub async fn apply_swap_fee(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    trade_id: Uuid,
    fee_amount: Decimal,
    occurred_at: DateTime<Utc>,
) -> anyhow::Result<Option<Decimal>>
```

`event_type='swap_fee'`、符号両対応 (apply_sfd_fee と同じく `fee_amount` 正=払い・balance 減 / 負=受取・balance 増)。

**注意**: paper swap での受取 case (`fee_amount < 0`) は、`apply_*_fee` の現状コード上 `trades.fees += fee_amount` で fees が **負** になりうる。bitFlyer SFD で既に同じ符号規約 (PR #92 で容認済)。

### 4. Cron 統合 (`crates/app/src/main.rs::overnight_handle`)

既存 task に GMO FX 経路を追加:

```rust
// 既存: bitFlyer 経路
if exchange == Exchange::BitflyerCfd {
    // entry_price × quantity × fee_rate
    ...
    apply_overnight_fee(...);
}

// 新規: GMO FX 経路
if exchange == Exchange::GmoFx {
    let pair_key = trade.pair.0.as_str();
    let Some(rate) = swap_config.rates.get(pair_key) else {
        // 未登録 pair は skip + debug log
        tracing::debug!("gmo swap: no rate for {} on trade {}, skip", pair_key, trade.id);
        continue;
    };
    let fee = compute_daily_swap(*rate, trade.direction, trade.quantity);
    if fee.is_zero() { continue; }
    apply_swap_fee(tx, account_id, trade.id, fee, event_at);
}
```

- timing は既存と同じ UTC date change (= JST 9:00、daily 1 回)
- `event_at` は `now_utc.date().midnight().and_utc()` (= UTC midnight) で attribution
- bitFlyer / GMO どちらの paper account も無い場合の早期 skip ロジックは既存通り

### 5. Migration

`migrations/20260519000001_account_events_add_swap_fee.sql`:

```sql
ALTER TABLE account_events
    DROP CONSTRAINT IF EXISTS account_events_event_type_check;
ALTER TABLE account_events
    ADD CONSTRAINT account_events_event_type_check
    CHECK (event_type IN (
        'margin_lock', 'margin_release', 'trade_open', 'trade_close',
        'overnight_fee', 'balance_sync', 'sfd_fee', 'swap_fee'
    ));
```

### 6. Dashboard / UI

- **`db/src/dashboard.rs` balance history**: event_type filter に `'swap_fee'` 追加 (`'trade_close', 'overnight_fee', 'sfd_fee'` に並ぶ形)
- **`db/src/trades.rs::TradeEventKind`**: `SwapFee` variant 追加 + `get_trade_events` の `match` に分岐追加
- **`dashboard-ui/src/api/types.ts`**: `TradeEvent.kind` union に `'swap_fee'` 追加
- **`dashboard-ui/src/components/TradeTable.tsx`**: `eventLabel`/`eventColor`/`renderCashDelta` に `'swap_fee'` 分岐追加 (label: "swap")

## Data Flow

```
overnight cron (daily UTC midnight)
  ↓
list_all accounts → filter paper
  ↓
for each paper trade:
   ├─ bitFlyer Crypto CFD: existing apply_overnight_fee path
   └─ GMO FX:
        rate = config.gmo_fx_swap.rates[pair]  (skip if None)
        fee  = compute_daily_swap(rate, direction, quantity)
        if fee != 0: apply_swap_fee(tx, account, trade, fee, event_at)
```

## Error Handling

| シナリオ | 挙動 |
|---------|------|
| config 未登録 pair | debug log + skip (config 拡張で対応想定) |
| `apply_swap_fee` per-trade 失敗 | error log + 進行 (PR A best-effort と同じ) |
| `list_all` accounts 失敗 | error log + continue (next tick で retry) |
| 全 paper account に target trade 無し | early skip (PR A round-12/15 と同パターン) |
| trade closed during tick | CAS skip (`Ok(None)`) |

## Testing

### Unit tests (`crates/core/src/swap.rs`)
- `compute_daily_swap_long_positive_rate` (USD_JPY Long 1 lot → +100)
- `compute_daily_swap_short_negative_rate` (USD_JPY Short 1 lot → -120)
- `compute_daily_swap_truncates_fractional_yen` (0.5 lot × +100 = 50 で整数)
- `compute_daily_swap_zero_for_zero_rate` (config rate 0 → 0)

### Integration tests (`crates/integration-tests/tests/phase3_gmo_swap_accrual.rs` 新規)
- paper GMO USD_JPY Long 1 lot → `Trade.fees -= 100` + `balance += 100` (受取)
- paper GMO USD_JPY Short 1 lot → `Trade.fees += 120` + `balance -= 120` (支払い)
- paper GMO 未登録 pair (例 GBP_JPY) → fees / balance 不変
- live GMO 対象外 (account_type filter で skip)
- paper bitFlyer 対象外 (exchange filter で skip)
- closed trade CAS skip

### Regression
- `phase3_jobs.rs` / `phase3_integrity.rs` の既存 overnight_fee テストは変更なし (bitFlyer 経路に影響なし)

## Scope of Change

### Modified
- `crates/core/src/config.rs` — `GmoFxSwapConfig`, `SwapRateEntry`, `AppConfig.gmo_fx_swap`
- `crates/core/src/lib.rs` — `pub mod swap;`
- `crates/db/src/trades.rs` — `apply_swap_fee` + `TradeEventKind::SwapFee` + `get_trade_events` 分岐
- `crates/db/src/dashboard.rs` — balance history event_type filter
- `crates/app/src/main.rs` — overnight_handle に GMO FX 経路追加
- `dashboard-ui/src/api/types.ts` — TradeEvent kind union 拡張
- `dashboard-ui/src/components/TradeTable.tsx` — swap_fee 表示分岐
- `config.toml` (sample) — `[gmo_fx.swap.rates]` セクション

### Created
- `crates/core/src/swap.rs`
- `crates/integration-tests/tests/phase3_gmo_swap_accrual.rs`
- `migrations/20260519000001_account_events_add_swap_fee.sql`

### Not modified
- `crates/market/src/gmo_fx_private.rs` (live API 経路触らない)
- bitFlyer overnight_fee logic (既存ロジック維持、GMO 経路は条件分岐で並列)

## Future PRs

- 3 倍デー対応 (NY 火曜→水曜 roll の 3 日分計上、土日対策)。config に `weekday_multiplier` table を追加して `compute_daily_swap` を拡張。
- restart 跨ぎ persistence (PR A round-17 で deferred、schema 変更を要するため別 PR)。
- (apply_*_fee 共通化 refactor は本 PR で対応済、`apply_fee_inner` 抽出)
- live swap 反映 (GMO API 仕様確認後、`GmoAccountAssets.total_swap` の delta tracking 等)。
