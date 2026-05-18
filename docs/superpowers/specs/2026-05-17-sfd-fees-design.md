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

`crates/market/src/exchange_api.rs` の `ExchangeApi` trait に **別 method を 1 本追加**:

```rust
/// Return accumulated SFD (Swap For Difference, bitFlyer Crypto CFD only)
/// for the given product at close time. Default impl returns 0 — only
/// exchanges that charge SFD override.
async fn fetch_close_sfd(&self, _product_code: &str) -> anyhow::Result<Decimal> {
    Ok(Decimal::ZERO)
}
```

**設計判断の経緯**: 当初は `resolve_position_id` を `resolve_position` に拡張
して `{id, sfd}` を 1 call で返す案を検討したが、以下の理由で却下:

1. bitFlyer は `requires_close_position_id() = false`、`resolve_position_id`
   は trader 側から **そもそも呼ばれない** (`trader.rs:850-859`)。よって
   ここに sfd を載せても bitFlyer の SFD は取得経路に乗らない。
2. GMO FX は sfd=0 (SFD 概念なし)。`resolve_position_id` を拡張する設計上の
   メリットが消える。
3. SFD が non-zero になるのは bitFlyer のみ。bitFlyer 専用に新規 API call が
   要る点は不可避なので、別 method を独立に追加する方が API 設計として
   素直 (concern が分離される)。

- **bitFlyer**: `fetch_close_sfd` を override。`get_positions(product_code)`
  を呼び、返却された position 群の `sfd` field を合計して返す
  (`position.sfd` field は既存)。
- **GMO FX / NullExchangeApi**: default 実装 (Ok(0)) を使用。
- `resolve_position_id` / `requires_close_position_id` は **変更なし**。

### Close 経路統合

`crates/executor/src/trader.rs::close_position`:

- `fill_close` の直前 (Phase 2 内、`fill_close_with_stale_recovery` も含む)
  で SFD を取得:
  - `dry_run`: `core::sfd::estimate(self.exchange, fill_price, qty)` (常に 0)
  - live: `self.api.fetch_close_sfd(&trade.pair.0)` を 1 度呼ぶ。
- 取得した SFD を `close_commission` と合算して `Trade.fees` に積む:
  ```rust
  let total_close_fee = close_commission + sfd_accrued;
  let closed_trade = Trade {
      ...
      fees: trade.fees + total_close_fee,
      ...
  };
  ```
- **CRITICAL bug fix (in-scope)**: 現状 `trader.rs:1119` は
  `update_trade_closed(..., trade.fees)` を渡しており、`closed_trade.fees`
  (= `trade.fees + close_commission`) ではなく `trade.fees` (= open
  commission のみ) が DB に書かれる。in-memory `closed_trade` には正しい
  値が乗っているため event/Slack 経路では見えるが、DB 行は close-time fee
  を取りこぼす。commission が現状全 exchange 0 なので顕在化していない
  latent bug。SFD は exchange API の実値が来るためこの bug が顕在化する。
  → `update_trade_closed(..., closed_trade.fees)` に修正する。

### bitFlyer SFD 取得失敗時の扱い

`fetch_close_sfd` が `Err` を返した場合 (rate limit / network 等):
- `dry_run`: 起こらない (paper は estimate を呼ぶだけ)
- live: warn ログを出し、SFD = 0 として close を続行する (close をブロック
  しない方が運用上安全 — SFD 未反映は手動補正可能だが close 失敗は
  liquidation リスクに直結)。

### 多重ポジション attribution の制限 (v1 limitation)

bitFlyer は同一 `product_code` に対する建玉を内部 netting する。同一
account-strategy で複数 trade が同時に open している場合、
`get_positions(product_code).sfd` の合計値はそれら全 trade の SFD を含む。
v1 では「close 時点での全 SFD 合計を closing trade に attribute する」
**FIFO 風の単純積算** とする。これにより:

- 同時 open 1 件のみのケース (本プロジェクトの bitFlyer paper account
  での運用パターン) では正確。
- 同時 open 複数件のケースでは、最初に close した trade に全 SFD が
  乗り、後続 close は SFD=0 となる (二重計上は起きない)。
- 将来精緻な attribution が必要になった時点で `Trade.sfd_accrued`
  column と reconcile job を追加する (Future PR セクション参照)。

## Data Flow

```
SL/TP/Manual/Liquidation 検知
 ↓
Trader::close_position(trade_id, exit_reason)
 ├─ Phase 1: acquire_close_lock (CAS)
 ├─ Phase 2: fill_close → (exit_price, close_commission)
 │            ↓ (NEW)
 │           fetch SFD:
 │            ├─ dry_run: sfd::estimate(exchange, price, qty) = 0
 │            └─ live   : api.fetch_close_sfd(product_code)
 │                          ├─ bitFlyer: Σ get_positions(product_code).sfd
 │                          └─ GMO/Null: default 0
 │            ↓
 │           total_close_fee = close_commission + sfd_accrued
 ├─ Phase 3: DB tx
 │            ├─ update_trade_closed(.., closed_trade.fees)  [BUG FIX]
 │            ├─ release_margin
 │            └─ insert_trade_closed_notification
 └─ TradeEvent::Closed   (closer.rs 経由)                    [既存]
```

paper 経路は `fetch_close_sfd` を呼ばず `sfd::estimate` で 0 を加算するだけ
だが、同じ `close_position` を通って同じ Trade.fees ロジックを共有する。

## Error Handling

- **`fetch_close_sfd` 失敗 (live)**: warn ログ + SFD=0 で close を続行
  (上記「bitFlyer SFD 取得失敗時の扱い」)。
- **sfd field missing / 空文字**: 既存の `ExchangePosition.sfd: Decimal`
  field は文字列 → Decimal の serde-with-str で parse される。bitFlyer の
  実 API は `"0"` を返すので missing 想定は不要だが、得られなかった場合は
  parse error として呼び出し元 (`fetch_close_sfd`) で `?` 伝播 → 上の
  warn 経路へ。
- **paper 経路**: `sfd::estimate()` は infallible (常に 0)。fees 0 加算は
  no-op だが skeleton を通すことで「ここに将来 SFD が入る」のドキュメント
  になる。

## Testing

### Unit tests

- `crates/core/src/sfd.rs`:
  - 3 exchange (BitflyerCfd / GmoFx / Oanda) で常に 0 を返すことを確認。

- `crates/market/src/bitflyer_private.rs`:
  - wiremock で `/v1/me/getpositions` が `[{sfd: "100"}, {sfd: "50"}]` を返す
    状況で `fetch_close_sfd("FX_BTC_JPY")` が `dec!(150)` を返すことを確認。
  - empty list → `Decimal::ZERO`。
  - HTTP 5xx → `Err` を返す (上位の warn 経路で扱う)。

- `crates/market/src/gmo_fx_private.rs`:
  - default 実装が `Ok(Decimal::ZERO)` を返すことを確認 (override していない
    ことの regression guard)。

### Integration tests

`crates/integration-tests/tests/phase3_sfd_close.rs` を新設 (5 ケース):

1. **bitFlyer close + sfd=100, live**: mock API が `fetch_close_sfd → 100`
   を返す → `closed.fees == open_commission + close_commission + 100`、
   かつ **DB から再 fetch した Trade.fees も同値** (commission bug fix
   regression guard 兼)。
2. **bitFlyer close + sfd=0, live**: `fetch_close_sfd → 0` → fees に SFD
   加算なし、DB も一致。
3. **GMO FX close, live**: default 0 → fees に SFD 加算なし、DB も一致。
4. **paper close**: `sfd::estimate=0` 経由で 0 が積まれる → fees 不変、
   DB も一致。
5. **bitFlyer SFD fetch 失敗**: mock が 503 を返す → warn ログ + SFD=0 で
   close が成功する (close は止まらない)、`closed.fees` は SFD 抜きで確定。

### Regression coverage

`phase3_close_flow.rs` / `phase3_commission.rs` / `phase3_gmo_close_handoff.rs`
等の既存 close flow テストは API 追加 (default 実装あり) のため変更不要。
ただし `phase3_commission.rs` の `live_close_accumulates_commission_on_top_of_open`
は DB 再 fetch アサーション (`get_trade(closed.id).fees == 100`) を追加して、
今回の `update_trade_closed` bug fix が将来再発しないようガードする。

## Scope of Change

### Modified files

- `crates/core/src/lib.rs` — `pub mod sfd;` 追加
- `crates/core/src/sfd.rs` — 新規 (estimate 関数のみ)
- `crates/market/src/exchange_api.rs` — `fetch_close_sfd` default method 追加
- `crates/market/src/bitflyer_private.rs` — `fetch_close_sfd` を override
  (`get_positions` 経由)
- `crates/executor/src/trader.rs`:
  - `close_position` Phase 2 末で sfd を取得して fees に積む
  - **bug fix**: `update_trade_closed(..., closed_trade.fees)` に修正
- `crates/integration-tests/tests/phase3_sfd_close.rs` — 新規 (5 ケース)
- `crates/integration-tests/tests/phase3_commission.rs` — DB 再 fetch
  アサーションを 1 行追加 (commission bug fix の regression guard)

### Not modified

- `crates/app/src/closer.rs` (interface 変更なし)
- `resolve_position_id` / `requires_close_position_id` (関係なし)
- DB schema (`Trade.fees` 既存 column 流用)

### Not modified (一覧、上の "Not modified" と併記)

- `commission` モジュール (独立、別軸)
- paper trader の execute / fill 経路 (close 経路の estimate 呼び出しのみ)

## Future PRs

1. **paper SFD 実計算**: bitFlyer public ticker `/v1/ticker?product_code=BTC_JPY`
   を polling して BTC 現物価格を保持、`abs(FX_BTC_JPY - BTC_JPY) / BTC_JPY`
   が閾値超で SFD を bot 側計算 → `sfd::estimate` を 0 ではなく実値に。
2. **Daily SFD snapshot**: cron で open position の sfd を log / Slack 通知し、
   operator が日次で追えるようにする。
3. **`Trade.sfd_accrued` 専用 column**: dashboard で commission と SFD を
   分離表示したい場合に schema 拡張。
