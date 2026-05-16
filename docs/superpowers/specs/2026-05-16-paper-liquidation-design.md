# Paper liquidation (アカウント単位の維持率ロスカット) 設計

- 作成日: 2026-05-16
- ステータス: 設計完了 (実装は次の plan で着手)
- 目的: paper=live 監査の残課題 #4 (paper liquidation) を解消。paper 経路でも live と同じ「アカウント維持率が `liquidation_margin_level` を下回ったら同 account の全 open trade を強制決済」挙動を実現する。
- 関連: PR #86 (GMO FX Private API) / PR #87 (commission) / `memory/feedback_paper_equals_live_in_unified_design.md`

## 背景

`Unified Trader` の paper / live 契約上、両経路は同じ計算結果になるはずだが、live は exchange (bitFlyer / GMO FX) が**アカウント単位の維持率がしきい値を下回ったら全 position を強制決済**する一方、paper にはこのロジックが存在しない。

監査結果:
- `crates/executor/src/position_sizer.rs:65-80` で `liquidation_margin_level` を使った post-SL margin level cap が存在し、**新規 open** のサイズが threshold を下回らないようにはなっている (open 前)。
- しかし、**既に open している position が逆行して維持率が threshold を下回った場合の force-close** ロジックは paper 経路に無い。SL/TP/StrategyTimeLimit の判定はあるが、margin level による液撤き判定は無い。
- live はこの判定を exchange 側で自動的に行う → paper だけが「永久に持てる」状態になる → P&L シミュレーションが live と大きく乖離する。

`liquidation_margin_level` の値 (`[exchange_margin.<exchange>]` 設定):
- `bitflyer_cfd`: 0.50 (50%、bitFlyer Crypto CFD 公式)
- `gmo_fx`: 1.00 (100%、GMO 外国為替 FX 公式)

これら threshold を paper でも適用する。

## ゴール

paper account (`account_type == "paper"`、または `LIVE_DRY_RUN=1` で paper モードに落ちている live account) に対し:

1. price tick 受信時に当該 trade の account の維持率を計算。
2. 維持率 `< liquidation_margin_level` なら、同 account の **全 open trade を `ExitReason::Liquidation` で順次 close** する。
3. live account (`account_type == "live"` かつ `LIVE_DRY_RUN` 未設定) は対象外。exchange 側に任せる。

## 非ゴール (この PR では触らない)

- live reconciler から同じ計算を呼び出す統合
- swap / SFD / overnight fee の維持率算入 (これらは別 PR で `account_events` / `Trade.fees` から差し引き済の `current_balance` で間接反映)
- 部分ロスカット (一部 trade だけ close する exchange もあるが、ここでは全 close で統一)
- Slack 通知の新カテゴリ追加 (既存 `PositionClosed` 通知の `exit_reason="liquidation"` で代用)
- 維持率がちょうど境界の場合の `==` 発火 (厳密に `<` のみ発火)

## Architecture

paper の position monitor (`crates/app/src/main.rs:836-1000` 周辺の crypto monitor) に **アカウント維持率判定**を追加。tick が来た `event.exchange + event.pair` に対し、同 account の全 open trade を walk して unrealized pnl を合算、`maintenance_ratio = (current_balance + Σrequired_margin + Σunrealized_pnl) / Σrequired_margin` を計算、`< liquidation_margin_level` なら同 account の全 trade を `ExitReason::Liquidation` で順次 close する。`current_balance` は margin lock 後の free cash なので、純資産算出時に lock 額 (= Σrequired_margin) を加算して initial-balance ベースに戻している点に注意。

live 経路は早期スキップ。既存の SL/TP/TimeLimit 判定の**直前** (`acquire_close_lock` より前) に置く ─ Liquidation が走ったら他 trade は既に `status='closing'/'closed'` で、SL/TP 判定の `acquire_close_lock` が natural に skip する。

## Components

| File | 変更 |
|------|------|
| `crates/core/src/types.rs` | `ExitReason::Liquidation` variant を追加。`as_str` / `FromStr` も対応 |
| `crates/core/src/margin.rs` | **新規** pure 関数 `compute_maintenance_ratio(current_balance, positions) -> Option<Decimal>`。required_margin == 0 のとき `None` を返す |
| `crates/core/src/lib.rs` | `pub mod margin;` を追加 |
| `crates/app/src/main.rs` | crypto monitor (`836-1000`) に維持率判定 + force-close ループを追加 |
| `crates/integration-tests/tests/phase3_paper_liquidation.rs` | **新規** integration test (4 ケース) |

## `core/margin.rs` API

```rust
//! exchange-agnostic な維持率計算。pure 関数で IO 依存なし。

use crate::types::Direction;
use rust_decimal::Decimal;

pub struct OpenPosition {
    pub direction: Direction,
    pub entry_price: Decimal,
    pub current_price: Decimal,
    pub quantity: Decimal,
    pub leverage: Decimal,
}

impl OpenPosition {
    pub fn unrealized_pnl(&self) -> Decimal {
        let diff = match self.direction {
            Direction::Long => self.current_price - self.entry_price,
            Direction::Short => self.entry_price - self.current_price,
        };
        diff * self.quantity
    }

    pub fn required_margin(&self) -> Decimal {
        // 必要証拠金 = entry_price × quantity / leverage
        // (= position notional / leverage)
        self.entry_price * self.quantity / self.leverage
    }
}

/// 維持率 = 純資産 / 必要証拠金合計。
/// 純資産 = current_balance(free cash) + Σrequired_margin(lock 戻し)
///         + Σunrealized_pnl
/// 必要証拠金合計が 0 (open trade 無し) のとき `None`。
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
```

## main.rs での組み立て (擬似コード)

```rust
// crypto monitor の price tick ループ内、既存 SL/TP 判定の前に追加
let tick_accounts: HashSet<Uuid> = open_trades
    .iter()
    .filter(|t| t.trade.exchange == event.exchange && t.trade.pair == event.pair)
    .map(|t| t.trade.account_id)
    .collect();

for account_id in &tick_accounts {
    // live skip
    let account_type = open_trades.iter().find(|t| t.trade.account_id == *account_id)
        .and_then(|t| t.account_type.as_deref()).unwrap_or("paper");
    let dry_run = account_type == "paper" || live_forces_dry_run;
    if !dry_run { continue; }

    // account row 読み込み
    let account = match auto_trader_db::trading_accounts::get_account(&pool, *account_id).await {
        Ok(Some(a)) => a,
        _ => { tracing::warn!("liquidation: account {} not found, skipping", account_id); continue; }
    };

    // 同 account の全 open trade を OpenPosition に変換
    let mut positions = Vec::new();
    let mut skip_account = false;
    for owned in open_trades.iter().filter(|t| t.trade.account_id == *account_id) {
        let trade = &owned.trade;
        let feed_key = FeedKey::new(trade.exchange, trade.pair.clone());
        let current_price = match price_store.latest_bid_ask(&feed_key).await {
            Some((bid, ask)) => match trade.direction {
                Direction::Long => bid,   // close-side
                Direction::Short => ask,
            },
            None => { tracing::warn!("liquidation: no price for {} {} — skipping account {}",
                                     trade.exchange, trade.pair, account_id);
                       skip_account = true; break; }
        };
        positions.push(OpenPosition { direction: trade.direction, entry_price: trade.entry_price,
                                       current_price, quantity: trade.quantity, leverage: trade.leverage });
    }
    if skip_account { continue; }

    // 維持率計算
    let ratio = match compute_maintenance_ratio(account.current_balance, &positions) {
        Some(r) => r,
        None => continue, // required=0、open 無し
    };
    let threshold = liquidation_level_or_log(&exchange_liquidation_levels, event.exchange, ...);
    if ratio >= threshold { continue; }

    // 発火: 同 account の全 trade を順次 close
    tracing::warn!("liquidation: account {} maintenance_ratio={} < threshold={} — force-closing all trades",
                   account_id, ratio, threshold);
    let trade_ids: Vec<Uuid> = open_trades.iter()
        .filter(|t| t.trade.account_id == *account_id)
        .map(|t| t.trade.id).collect();
    for trade_id in trade_ids {
        // Trader を組み立てて close_position(trade_id, ExitReason::Liquidation)
        // (既存 SL/TP close と同じパスを再利用、新規構造体不要)
        // close 失敗時は warn + 次の trade に進む (1 trade の失敗で残りを止めない)
    }
}
```

`Trader::close_position` 呼び出しは既存の SL/TP close 経路と同じ。`ExitReason::Liquidation` を新 variant として追加、`as_str = "liquidation"`、`FromStr` も同じ string を受ける。

## ExitReason 拡張

```rust
// crates/core/src/types.rs:143
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
    Liquidation,  // ← 新規
}

// as_str:
ExitReason::Liquidation => "liquidation",

// FromStr:
"liquidation" => Ok(ExitReason::Liquidation),
```

## Testing

### Unit (`crates/core/src/margin.rs`)

- `compute_maintenance_ratio_long_in_profit`: balance=100k, entry=150, current=151, qty=10000, lev=25 → required=60k, unrealized=10k → ratio=110000/60000≈1.833
- `compute_maintenance_ratio_short_in_loss`: balance=100k, entry=150, current=152, qty=10000, lev=25, Short → unrealized=-20000 → ratio=80000/60000≈1.333
- `compute_maintenance_ratio_multiple_positions_sum`: 複数 OpenPosition で required と unrealized が和になることを確認
- `compute_maintenance_ratio_zero_required_returns_none`: 空 vec → None
- `compute_maintenance_ratio_negative_equity`: balance + unrealized が負 → 比率も負 (caller の threshold 比較に使える)

### Integration (`crates/integration-tests/tests/phase3_paper_liquidation.rs`)

1. **`liquidation_fires_when_maintenance_drops_below_threshold`**:
   - paper GMO FX account を 100,000 JPY で seed
   - Long trade を 1 open (entry=150, qty=10000, lev=25 → required=60,000)
   - PriceStore を threshold 直前まで動かす (ratio 1.01) → close されない
   - PriceStore を threshold 下まで動かす (ratio 0.99) → `ExitReason::Liquidation` で close

2. **`liquidation_closes_all_trades_in_account`**:
   - 同 account に 2 open trades (USD_JPY と FX_BTC_JPY)
   - 一方の pair の price が大きく逆行 → 両 trade が `Liquidation` で close

3. **`live_account_skips_liquidation_judgment`**:
   - `account_type="live"` で同じ price 動きを起こしても close されない

4. **`missing_price_skips_judgment`**:
   - PriceStore に必要 pair の price 無し → 判定 skip (false-positive 防止)、close 発生せず

### Regression

既存 `phase3_close_flow` / `phase3_jobs` / `phase3_strategy_exit_e2e` は paper でも初期 balance が十分大きく Liquidation 発火条件に達しないため影響無し。

## Error handling

- `get_account` が `None` (account 削除レース) → warn log + 同 account の判定 skip
- `compute_maintenance_ratio` が `None` (required=0) → スキップ
- PriceStore に該当 pair の price 不在 → warn log + 該当 account の判定 skip (false-positive Liquidation を避ける)
- Liquidation 内の `close_position` 失敗 → warn log + 次の trade に進む (1 trade 失敗で残りを止めない、`release_close_lock` は close_position 内で行われる)
- 維持率がちょうど threshold (e.g. 1.000) → `< threshold` で厳密下回りのみ発火 (`==` では発火しない、live exchange の境界仕様に合わせる)

## マイグレーション・互換性

- DB schema 変更なし (`Trade.exit_reason` は文字列で柔軟、新 string `"liquidation"` を受け取る)
- 既存 trades 行はそのまま (新 `Liquidation` variant が `FromStr` の string と一致する)
- 既存 paper account / live account の挙動は paper のみで判定追加、live は変更無し
- 既存 strategy / SL/TP / TimeLimit は前後関係 (Liquidation → 既存 SL/TP) の 1 段先行で動き、既存 test に regression 無し

## レビュー観点 (PR description に含める想定)

- account 単位の維持率 (= live exchange 等価)
- 全 open trade を順次 close (1 trade の失敗で残りを止めない)
- price 不在時の false-positive 防止
- 厳密下回り (`<`) のみ発火
- `core/margin.rs` の pure 関数化で test 容易
- live 経路は対象外 (exchange に任せる)
