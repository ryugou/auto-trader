# SFD (Swap For Difference) Fees Design

## Goal

bitFlyer Crypto CFD で発生する **SFD (現物-FX 乖離手数料)** を close 時に
`Trade.fees` に正しく積算する。paper account は estimate 0 固定 (commission
PR #87 と同パターン)。これにより paper=live contract の "fees" 軸の整合性が回復する。

## Context

- bitFlyer Crypto CFD の SFD は **BTC 現物価格と FX_BTC_JPY 価格の乖離が一定
  以上 (通常 5%) で発生**し、position に cumulative に課金される。
- `crates/market/src/bitflyer_private.rs` の `ExchangePosition` 構造体には
  既に `pub sfd: Decimal` field があり、`/v1/me/getpositions` の response から
  パースしている。
- しかし `Trader::close_position` 経路では `Trade.fees` に sfd を反映していない。
  結果として bitFlyer 実取引で課金された SFD が ledger に乗らず、PnL が
  楽観的にズレる可能性がある。
- GMO Coin FX、paper account には SFD 概念はない。
- 4 bitFlyer paper account が現在 active (78 trades, latest 2026-05-16) で、
  paper→live 移行時に fees 軸の整合が要件。

## Non-Goals

- paper の SFD 実計算 (BTC spot feed 追加が必要、別 PR)。
- Daily SFD snapshot for operator monitoring (Slack 通知等)。
- `Trade.sfd_accrued` 専用 column の追加 (現状 `Trade.fees` 一本で十分)。
- SFD 発生条件の bot 側検知ロジック (exchange 側で計算した cumulative 値を
  そのまま信用する)。

## Architecture

### paper skeleton

`crates/core/src/sfd.rs` を新設。commission `crates/core/src/commission.rs`
と同形:

```rust
pub fn estimate(_exchange: Exchange, _fill_price: Decimal, _qty: Decimal) -> Decimal {
    Decimal::ZERO
}
```

将来 BTC spot feed を追加して paper でも実計算する際は、この 1 関数の中身
を差し替えるだけで paper/live 両方が追従する。

### Trait 拡張

`crates/market/src/exchange_api.rs` の `ExchangeApi` trait:

```rust
// Before
fn resolve_position_id(...) -> Result<Option<String>>;

// After
fn resolve_position(...) -> Result<Option<ResolvedPosition>>;

pub struct ResolvedPosition {
    pub id: String,
    pub sfd: Decimal,  // 累積 SFD。SFD 概念のない exchange は Decimal::ZERO
}
```

- bitFlyer: 既存の `get_positions` response から `id` と `sfd` を同じレスポンス
  内で取得 (redundant API call なし)。`sfd` field が missing/空文字なら
  `Decimal::ZERO`。
- GMO FX: `id` のみ返し `sfd: Decimal::ZERO`。
- paper exchange: `None` を返す既存挙動を維持 (paper は position_id 不要)。

`requires_close_position_id` は変更なし。

### Close 経路統合

`crates/executor/src/trader.rs::close_position`:

- `resolve_position_id_with_retry` → `resolve_position_with_retry` に rename。
  戻り値は `Option<ResolvedPosition>`。retry frame は既存をそのまま流用。
- `resolved.sfd` を `crates/app/src/closer.rs::close_trade` まで持ち回す。
- `close_trade` 内で `Trade.fees += resolved.sfd` を 1 度だけ加算してから
  DB を update。
- paper 経路は `core::sfd::estimate(...)` を呼んで 0 を fees に積む (skeleton
  を明示的に通す。将来 spot feed 化に備える)。

## Data Flow

```
SL/TP/Manual/Liquidation 検知
 ↓
resolve_position_with_retry()                [既存 retry frame 流用]
 ├─ bitFlyer: get_positions → {id, sfd=累積}
 ├─ GMO FX  : send_child_order/openPositions → {id, sfd=0}
 └─ paper   : None
 ↓
close_position(id) → exchange API           [既存]
 ↓
close_trade(trade, fill_price, exit_reason, commission=resolved.sfd)
 ├─ Trade.fees += commission                [新規: cumulative SFD を反映]
 └─ DB.update Trade.fees
 ↓
TradeEvent::Closed                          [既存]
```

paper 経路は `commission` 引数に `sfd::estimate(...)`=0 を渡し、同じ
`close_trade` を通る。

## Error Handling

- **resolve 失敗**: 既存挙動と同じ (close skip、リトライ枯渇で warn ログ)。
  SFD は cumulative なので、resolve 成功 = sfd も同時に取れている、が
  invariant として成立する (同じ API response から抽出するため)。
- **sfd field missing / 空文字**: serde default で `Decimal::ZERO`。
  bitFlyer API の現実的な response 形状に合わせる。
- **paper 経路**: `estimate()` は infallible (常に 0)。fees 0 加算は no-op
  だが skeleton を通すことで「ここに将来 SFD が入る」のドキュメントになる。

## Testing

### Unit tests

- `crates/core/src/sfd.rs`:
  - 3 exchange (BitflyerCfd / GmoFx / Oanda) で常に 0 を返すことを確認。

- `crates/market/src/bitflyer_private.rs`:
  - mock JSON で `sfd: "1234.5"` → `Decimal::new(12345, 1)` にパース。
  - mock JSON で `sfd: ""`/missing → `Decimal::ZERO`。
  - `resolve_position` が `{id, sfd}` を正しく組み立てることを確認。

### Integration tests

`crates/integration-tests/tests/phase3_sfd_close.rs` を新設 (4 ケース):

1. **bitFlyer close + sfd=100**: mock server が sfd=100 を返す状況で close →
   `Trade.fees` に 100 が加算される。
2. **bitFlyer close + sfd=0**: mock server が sfd=0 を返す状況 → `Trade.fees`
   不変 (commission 等の他要素のみ反映)。
3. **GMO FX close**: SFD 概念がない → `Trade.fees` に SFD 加算されない。
4. **paper close**: `sfd::estimate()` 経由で 0 が積まれる → `Trade.fees`
   不変 (skeleton を通る確認のためのテスト)。

### Regression coverage

`phase3_close_flow.rs` / `phase3_gmo_close_handoff.rs` 等の既存 close flow
テストは `resolve_position_id` → `resolve_position` の rename に追随。挙動
変化はないことを assert で固める (`Trade.fees` が sfd=0 のとき変化しない)。

## Scope of Change

### Modified files

- `crates/core/src/lib.rs` — `pub mod sfd;` 追加
- `crates/core/src/sfd.rs` — 新規
- `crates/market/src/exchange_api.rs` — trait method rename + `ResolvedPosition`
  struct 追加
- `crates/market/src/bitflyer_private.rs` — `resolve_position` 実装、sfd を含む
- `crates/market/src/gmo_fx_private.rs` — `resolve_position` 実装、sfd=0
- `crates/executor/src/trader.rs` — `resolve_position_with_retry` rename、
  `close_position` 経路で `resolved.sfd` を `close_trade` へ持ち回す
- `crates/app/src/closer.rs` — `close_trade` の既存 `commission: Decimal`
  引数に `commission + sfd` を合算して渡す (DB 上は `Trade.fees` 一本のため
  同じ加算操作。将来 `Trade.sfd_accrued` を分離したくなったら column 化と
  同時に引数も分離する)
- `crates/integration-tests/tests/phase3_sfd_close.rs` — 新規
- 既存 phase3 テスト群 — rename 追随

### Not modified

- DB schema (`Trade.fees` 既存 column 流用)
- `commission` モジュール (独立、別軸)
- `requires_close_position_id` (変更なし)
- paper trader の execute / fill 経路 (close 直前の estimate 呼び出しのみ)

## Future PRs

1. **paper SFD 実計算**: bitFlyer public ticker `/v1/ticker?product_code=BTC_JPY`
   を polling して BTC 現物価格を保持、`abs(FX_BTC_JPY - BTC_JPY) / BTC_JPY`
   が閾値超で SFD を bot 側計算 → `sfd::estimate` を 0 ではなく実値に。
2. **Daily SFD snapshot**: cron で open position の sfd を log / Slack 通知し、
   operator が日次で追えるようにする。
3. **`Trade.sfd_accrued` 専用 column**: dashboard で commission と SFD を
   分離表示したい場合に schema 拡張。
