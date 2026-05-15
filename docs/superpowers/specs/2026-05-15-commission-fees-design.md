# Commission を Trade.fees に記録する 設計

- 作成日: 2026-05-15
- ステータス: 設計完了 (実装は次の plan で着手)
- 目的: 「paper = live 契約」の残課題 #2 (commission) を解消し、将来 commission 仕様が変わっても paper と live の Trade.fees が divergence しないようにする。
- 関連: `memory/feedback_paper_equals_live_in_unified_design.md`、PR #86 (paper=live 契約 1/N) のフォロー

## 背景

`Unified Trader` (`crates/executor/src/trader.rs`) は paper / live を同じコード経路で動かし、`dry_run` フラグだけで分岐する設計。しかし現状 `Trader::execute` で `Trade.fees: Decimal::ZERO` がハードコードされており、live で `Execution.commission` が返ってきても DB に積まれていない。

各取引所の現状の公式 commission は事実上 0 (GMO FX は spread のみ、bitFlyer Crypto CFD は基本 0、OANDA はマージン取引で 0)。よって現状の paper と live の `Trade.fees` はどちらも 0 で一致しており、純粋なバグというより「**将来 silent break 用意**」の性質の実装漏れ。

将来取引所側が commission を 0 以外に上げた場合、live は API 値で動くが paper はハードコード 0 のまま、という divergence が発生する。今のうちに API 値を拾うコード + paper の commission 計算を 1 か所に集中させる構造を入れておく。

## ゴール

1. live (`dry_run=false`) で `fill_open` / `fill_close` の各約定の `Execution.commission` を集計し、`Trade.fees` に累積する。
2. paper (`dry_run=true`) で同じ commission を `commission::estimate_open` / `commission::estimate_close` 関数経由で計算し、`Trade.fees` に累積する。
3. 現状の estimate 関数の戻り値は全 Exchange で 0。将来仕様変更があったらこの 1 ファイルだけを更新する運用にする。
4. `Trade.fees` の累積方法は既存 (`apply_overnight_fee` で increment) と一貫させる。

## 非ゴール (この PR では触らない)

- bitFlyer SFD (Swap For Difference) の計算 (別 PR、別ファクト)
- `Trade.open_fees` / `Trade.close_fees` の分割 (累積で十分)
- commission 実レートの更新 (現状 0 のまま固定。実取引で commission を観測したら別途)
- DB schema 変更 (`Trade.fees` は既存)
- 既存 overnight_fee job との統合

## Architecture

```
Trade.fees = open_commission + close_commission ( + 既存の overnight_fee 累積)

[live, dry_run=false]
  fill_open  → poll_executions → Vec<Execution>.commission.sum() = open_commission
  fill_close → poll_executions → Vec<Execution>.commission.sum() = close_commission

[paper, dry_run=true]
  fill_open  → commission::estimate_open(exchange, fill_price, qty)   = open_commission
  fill_close → commission::estimate_close(exchange, fill_price, qty)  = close_commission
```

paper と live はどちらも同じ `Trade.fees` フィールドに最終的に積まれる。違いは「commission の供給元」だけ — live は API レスポンス、paper は static table。両方とも 0 を返している現状では結果同一、将来 commission が上がった時は `commission.rs` の 1 ファイル更新 (live は API が自動追従、paper は estimate 関数を更新) で両方等価のまま保たれる。

## Components

| File | 変更 | 内容 |
|------|------|------|
| `crates/core/src/commission.rs` | **新規** | `estimate_open(Exchange, Decimal, Decimal) -> Decimal` / `estimate_close(...)`、Exchange enum を exhaustive match で扱う |
| `crates/core/src/lib.rs` | +1 行 | `pub mod commission;` |
| `crates/executor/src/trader.rs` | 変更 | `fill_open` の戻り値 `(Decimal, Decimal)` → `(Decimal, Decimal, Decimal)` 第三戻り値が commission。`fill_close` も `Decimal` → `(Decimal, Decimal)` に拡張。`Trader::execute` で `Trade.fees = open_commission` を設定、`Trader::close_position` で `closed_trade.fees = trade.fees + close_commission` |
| `crates/integration-tests/tests/phase3_commission.rs` | **新規** | live 経路で `Execution.commission` が累積されることを mock で確認 |

`fill_open` / `fill_close` のシグネチャ変更は呼び出し側 (`execute` / `close_position` / `fill_close_with_stale_recovery` / `fill_close_size`) も合わせて更新が必要。

## commission.rs API

```rust
//! exchange 別の公式 commission を計算する pure 関数群。
//!
//! 現状 全 Exchange で 0 を返す (各取引所の現実の commission レートを反映)。
//! 将来 commission レートが変わったらこのファイルを更新するだけで
//! paper 側の Trade.fees 計算が live と等価のまま保たれる。

use rust_decimal::Decimal;
use crate::types::Exchange;

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
```

exhaustive match を `_` arm 無しで書くので、新しい Exchange enum variant を追加した時に compile error で気づける (これは leverage validation でも採用した pattern と同じ)。

## fill_open / fill_close シグネチャ変更

```rust
// Before
async fn fill_open(&self, signal: &Signal, quantity: Decimal) -> anyhow::Result<(Decimal, Decimal)>;
async fn fill_close(&self, trade: &Trade) -> anyhow::Result<Decimal>;
async fn fill_close_size(&self, trade: &Trade, size: Decimal) -> anyhow::Result<Decimal>;
async fn fill_close_with_stale_recovery(&self, trade: &Trade) -> anyhow::Result<(Decimal, bool)>;

// After
async fn fill_open(&self, signal: &Signal, quantity: Decimal) -> anyhow::Result<(Decimal, Decimal, Decimal)>; // (price, qty, commission)
async fn fill_close(&self, trade: &Trade) -> anyhow::Result<(Decimal, Decimal)>;                              // (price, commission)
async fn fill_close_size(&self, trade: &Trade, size: Decimal) -> anyhow::Result<(Decimal, Decimal)>;
async fn fill_close_with_stale_recovery(&self, trade: &Trade) -> anyhow::Result<(Decimal, Decimal, bool)>;   // (price, commission, approximate)
```

`fill_close_with_stale_recovery` の `approximate=true` 経路では exchange position が既に gone のため commission を取得できない。その場合は 0 を返し、operator alert に "approximate exit price + zero commission fallback" の旨を含める (既存の alert 文言に追加)。

## DB layer

- `insert_trade`: 既存の `fees` field をそのまま使用 (insert 時に Trade.fees が commission 込み)。変更不要。
- `update_trade_closed`: 既存の `fees: Decimal` パラメータをそのまま使用。close 時の累積 fees を渡す。変更不要。
- `apply_overnight_fee`: 既存通り `fees = fees + delta` で動作。変更不要。

## Testing

### Unit (`commission.rs`)

```rust
#[test]
fn estimate_open_all_exchanges_currently_zero() {
    use rust_decimal_macros::dec;
    assert_eq!(estimate_open(Exchange::BitflyerCfd, dec!(150), dec!(1)), Decimal::ZERO);
    assert_eq!(estimate_open(Exchange::GmoFx, dec!(150), dec!(1)), Decimal::ZERO);
    assert_eq!(estimate_open(Exchange::Oanda, dec!(150), dec!(1)), Decimal::ZERO);
}

#[test]
fn estimate_close_all_exchanges_currently_zero() {
    // 同じく 3 件
}
```

### Integration (`crates/integration-tests/tests/phase3_commission.rs`)

新規 in-test mock `CommissionCaptureMockApi` が `get_executions` で commission を返す。テストケース:

1. `live_open_accumulates_open_commission_into_fees`: GMO mock が open execution で `commission="100"` を返す → 直後 `Trade.fees == 100`
2. `live_close_accumulates_close_commission_into_fees`: GMO mock が close execution で `commission="50"` を返す、open は commission=100 → close 後 `trade.fees == 150` (累積)
3. `paper_estimate_zero_for_all_exchanges`: dry_run=true で open + close → `trade.fees == 0` (estimate 関数が現状 0 を返すため)
4. (オプション) `bitflyer_commission_accumulates`: bitFlyer mock の equivalent

### Regression

- 既存 `phase3_close_flow.rs` / `phase3_jobs.rs` / `phase3_strategy_exit_e2e.rs` 等の `trade.fees` を参照しているテストは、paper のみで commission=0 のため動作変わらず。

## Error handling

- API レスポンスに `commission` field 欠落 → serde の Decimal default で 0 (現状の挙動を維持、追加変更なし)
- Negative commission (リベート) → そのまま積む。`Decimal` は signed なので計算上 fees から差し引かれる
- `Vec<Execution>` が空 → 0 (sum の natural fallback)
- `fill_close_with_stale_recovery` の approximate 経路 → commission 0 を best-effort で返す、既存の operator alert にその旨明記

## マイグレーション・互換性

- DB schema 変更なし
- 既存の DB row (fees=0) はそのまま使える
- 既存の paper 戦略の挙動は不変 (estimate が現状 0)
- 既存の live トレード時の挙動も commission=0 が API から返る限り不変

## レビュー観点 (PR description に含める想定)

- `fill_open` / `fill_close` のシグネチャ変更が漏れなく呼び出し側に反映されているか
- paper / live で同じ `Trade.fees` が出ることを integration テストで証明
- exhaustive match で新 Exchange 追加時の compile error を担保
