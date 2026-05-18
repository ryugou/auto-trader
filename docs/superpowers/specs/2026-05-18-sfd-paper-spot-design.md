# bitFlyer SFD Paper Accrual Design

## Goal

bitFlyer Crypto CFD の SFD (現物-FX 乖離手数料) を **paper account でも実計算**して `Trade.fees` に積算し、live の API 実値と等価な PnL になるようにする。これにより paper = live contract の最後の例外を解消し、レイテンシ起因 slippage 以外の差が出ない状態にする。

## Context

- PR #91 で SFD を **live** のみ `fetch_close_sfd` 経由で `Trade.fees` に反映済み。**paper は `estimate=0`** で取りこぼし、これが paper=live contract の唯一の挙動差として残っていた。
- bitFlyer Crypto CFD の SFD は **現物 (BTC_JPY) と FX_BTC_JPY の価格乖離率** に応じて、**時報** (毎時) で open position に課金される (公式仕様)。
- 既存の `apply_overnight_fee` が同じパターンを取っており、SFD 用の job を 1 本追加すれば対称的に実装できる。
- 既存の bitFlyer WebSocket task (`crates/market/src/bitflyer.rs`) は `pairs: Vec<Pair>` を受けて subscribe する設計で、subscribe pair に `BTC_JPY` (現物) を追加すれば spot tick が PriceStore に流れる。新規 polling task 不要。

## Non-Goals

- live trade に対する SFD 反映の変更 (PR #91 の `fetch_close_sfd` を維持)。double counting を避けるため、新規 hourly job は **paper のみ** 対象。
- GMO FX swap point (別 PR で対応)。
- bitFlyer 公式 SFD rate の動的取得 (rate 改定時は const 更新で追従)。
- SFD 乖離率 ≠ 0 だが取引量が極端に少ない時の真の「指数価格」取得 (BTC_JPY ticker で近似)。

## Architecture

### 1. Spot feed (既存 WS 拡張)

`crates/market/src/bitflyer.rs` の WS task の subscribe pair に `BTC_JPY` (現物) を加える。

- strategy 側は引き続き FX_BTC_JPY のみを使う。BTC_JPY tick は **PriceStore に流れるだけ** で signal source ではない。
- candle builder / strategy 経路には影響させない (BTC_JPY pair に対する CandleBuilder や Strategy をマウントしない)。
- bitFlyer Public WebSocket は `lightning_ticker_BTC_JPY` channel として無料で subscribe 可能。追加コストなし。

### 2. SFD 計算式 (公式階段 hardcode)

`crates/core/src/sfd.rs` に追加。既存の `estimate(...)` 関数は **互換維持で残す** (`Decimal::ZERO` を返し続ける):

- `estimate` は trader.rs::close_position の paper 経路で呼ばれている skeleton。**今回 hourly job が累積する**ため、close 時の estimate は 0 で正しい (二重計上回避)。

新規追加:

```rust
pub struct SfdContext {
    pub fx_price: Decimal,           // FX_BTC_JPY (latest tick)
    pub spot_price: Decimal,         // BTC_JPY (latest spot tick)
    pub position_notional: Decimal,  // entry_price × quantity
    pub direction: Direction,        // Long / Short
}

/// bitFlyer Crypto CFD 公式 SFD 階段 (**daily** rate)。
/// 内部で `.abs()` を取るため呼び出し側は符号を気にせず渡せる
/// (Copilot review round-3 で pub function の footgun を内部正規化で解消)。
///   |x| < 5%        → 0.00%
///   5%  ≤ |x| < 10% → 0.25%
///   10% ≤ |x| < 15% → 0.50%
///   15% ≤ |x| < 20% → 1.00%
///   20% ≤ |x|       → 3.00%
pub fn sfd_daily_rate(divergence: Decimal) -> Decimal;

/// hourly SFD = notional × (daily_rate / 24) × sign
///
/// sign 決定 (bitFlyer 仕様):
///   乖離率 > 0 (FX > spot) かつ Long  →  +fee (払う)
///   乖離率 > 0           かつ Short →  -fee (受け取る)
///   乖離率 < 0           かつ Long  →  -fee (受け取る)
///   乖離率 < 0           かつ Short →  +fee (払う)
///
/// 戻り値が正なら fee として `Trade.fees` に加算 + `current_balance` から減算、
/// 負なら fees を減算 + balance を増加。
pub fn compute_hourly_sfd(ctx: SfdContext) -> Decimal;
```

### 3. Hourly accrual job

`crates/app/src/main.rs` に既存 `overnight_fee` task と同パターンで追加:

```
loop {
    sleep_until next hour (毎時 0 分)
    for account in paper bitflyer accounts:
        for trade in get_open_trades_by_account(account):
            fx   = price_store.latest(FX_BTC_JPY).ok()?
            spot = price_store.latest(BTC_JPY).ok()?
            sfd  = compute_hourly_sfd({fx, spot, trade.notional, trade.direction})
            if sfd != 0:
                apply_sfd_fee(tx, account, trade, sfd)  // atomic
}
```

- `account_type = 'paper'` AND `exchange = 'bitflyer_cfd'` でフィルタ
- spot/fx tick が片方でも欠落 → 該当 tick は skip + warn ログ (close をブロックしない方が安全)
- 既存 `overnight_fee` cron との並列発火 OK (event_type で区別、CAS で安全)

### 4. DB 拡張

新規 helper `crates/db/src/trades.rs::apply_sfd_fee`:

`apply_overnight_fee` を base にコピー。違い:
- `event_type = 'sfd_fee'` で `account_events` 記録
- `fee_amount` は **正負両対応** (受け取り fee = balance 増加 + trades.fees 減算)
- CAS: `WHERE id=$1 AND account_id=$2 AND status='open'`

Migration `20260518000001_account_events_add_sfd_fee.sql`:

```sql
ALTER TABLE account_events
    DROP CONSTRAINT account_events_event_type_check;

ALTER TABLE account_events
    ADD CONSTRAINT account_events_event_type_check
    CHECK (event_type IN (
        'margin_lock', 'margin_release', 'trade_open', 'trade_close',
        'overnight_fee', 'balance_sync', 'sfd_fee'
    ));
```

### 5. Live は変更なし

PR #91 の `fetch_close_sfd` 経路を維持。

- live trade の SFD = close 時に API 実値 (1 回 read)
- paper trade の SFD = bot の hourly accrual (本 PR で新規)
- 完全に別経路で double counting なし

## Data Flow

```
bitFlyer WS (BTC_JPY tick + FX_BTC_JPY tick)
   ↓
PriceStore (FeedKey: BitflyerCfd × {BTC_JPY, FX_BTC_JPY})
   ↓
hourly cron (毎時 0 分)
   ↓
for each paper bitflyer trade:
    fx   = price_store.latest_bid_ask(FX_BTC_JPY)
    spot = price_store.latest_bid_ask(BTC_JPY)
    fee  = compute_hourly_sfd({fx, spot, notional, direction})
    apply_sfd_fee(tx, account, trade, fee)   // CAS + balance + event
```

## Error Handling

| シナリオ | 挙動 |
|---------|------|
| BTC_JPY tick 欠落 (WS 切断中) | 該当 tick は SFD skip + `warn` ログ。次の tick で復活 |
| FX_BTC_JPY tick 欠落 | 同上 |
| compute_hourly_sfd で notional 0 / leverage 0 | `Decimal::ZERO` を返して no-op |
| trade が hourly tick の間に close → CAS で skip | `apply_sfd_fee` が `Ok(None)` を返す (`apply_overnight_fee` 同様) |
| balance が SFD で 0 を下回る | balance < 0 を許容 (現状の overnight_fee と同じ。liquidation 監視は別 path) |

## Testing

### Unit tests (`crates/core/src/sfd.rs`)
- `sfd_daily_rate`: 階段境界 (0%, 4.99%, 5%, 9.99%, 10%, 15%, 20%, 25%) で正しい rate
- `compute_hourly_sfd`: BUY/SELL × 乖離 ±方向 4 通り + 0 ケース + 0% 乖離
- 戻り値の符号 (Long+FX>spot → 正、等)

### Integration tests (`crates/integration-tests/tests/phase3_sfd_paper_accrual.rs` 新規)

1. **paper bitFlyer + 乖離 10%, Long**: hourly job 1 回実行後、`Trade.fees` が `notional × 0.005/24` 増加。`current_balance` が同額減少。`account_events` に `sfd_fee` row 追加。
2. **paper bitFlyer + 乖離 4%**: SFD = 0、変化なし。
3. **paper bitFlyer + 乖離 10%, Short** (FX > spot): fee 負値、`Trade.fees` 減少、balance 増加。
4. **live bitFlyer 対象外**: live account を立てて同じ tick で job 実行 → fees 不変。
5. **GMO FX 対象外**: paper GMO account → 不変。
6. **trade closed during tick**: open → tick → close → 次 tick で CAS skip (二重計上なし)。

### Regression
- `phase3_sfd_close.rs` (PR #91 live close path) は影響なし。`estimate` は引き続き 0 を返す。
- `phase3_commission.rs` の DB persistence regression guard は引き続き有効。

## Scope of Change

### Modified
- `crates/market/src/bitflyer.rs` — WS subscribe pair に BTC_JPY 追加 + integration test mock の対応
- `crates/core/src/sfd.rs` — `SfdContext`, `sfd_daily_rate`, `compute_hourly_sfd` 追加 (既存 `estimate` 維持)
- `crates/db/src/trades.rs` — `apply_sfd_fee` 追加
- `crates/app/src/main.rs` — hourly SFD job 追加 (`overnight_fee` の隣)
- `migrations/20260518000001_account_events_add_sfd_fee.sql` — 新規 migration

### Created
- `crates/integration-tests/tests/phase3_sfd_paper_accrual.rs` — 6 ケース

### Not modified
- `Trade.fees` schema (既存 column 流用)
- live `fetch_close_sfd` 経路 (PR #91 のまま)
- `estimate` 関数 (skeleton 維持で 0)
- 他 exchange (GMO FX は PR B で対応)

## Implementation-time verification

実装フェーズで以下を必ず実コード/公式 docs と照合する:

1. **SFD sign 方向**: 本 spec では「FX > spot で Long 持ち → 払う」と記載したが、
   bitFlyer Crypto CFD 公式仕様で sign 方向 (どちら側が払うか) を再確認し、
   実装と spec の符号が一致することを確認する。誤っていれば spec も修正。
2. **`Trade.fees` の負値許容**: 受け取り SFD で `Trade.fees` が負になりうるが、
   `trades.fees` column 型 (Decimal) + 既存 CHECK 制約 (もしあれば) が負値を
   許容するか確認。許容しないなら migration で緩めるか、symbol を separate
   column で持つ設計に変える。
3. **hourly job の初回発火タイミング**: 既存 `overnight_fee` の loop パターン
   (起動 → 次の日付境界まで sleep → daily 発火) を踏襲する。起動直後の中途
   timing tick での即時計上は **行わない** (二重計上 / 部分時間計算回避)。

## Future PRs

- **PR B**: GMO FX swap point を overnight_fee/SFD と同パターンで実装
- bitFlyer 公式 SFD rate が改定された時の追従 (現状は const)
- 真の「指数価格」(複数取引所平均) 対応 — 現状は BTC_JPY ticker で代用
