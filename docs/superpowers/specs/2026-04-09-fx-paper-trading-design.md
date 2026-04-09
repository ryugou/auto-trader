# FX Paper Trading Enablement (OANDA demo / USD/JPY)

- 作成日: 2026-04-09
- 対象: Rust backend (auto-trader-app, auto-trader-market, auto-trader-strategy) + Postgres migration

## 目的

OANDA デモ口座経由で USD/JPY のペーパートレードを動かす。crypto 側 (bitflyer) と対称な実装にし、市場フィード健康監視・ポジションモニター・戦略・口座 seed を揃える。戦略は新規 `donchian_trend_fx_v1` を allocation_pct 違いで 2 インスタンス登録し、4 口座 (残高 × allocation のマトリクス) で並列検証する。

## スコープ

- OANDA streaming pricing API (`/v3/accounts/{id}/pricing/stream`) を実装して raw tick を PriceStore に流す
- `MarketMonitor` に `with_raw_tick_sink` を追加 (bitflyer と対称)
- FX ポジションモニター (`Exchange::Oanda` フィルタ) を復活させる
- 新規 FX 戦略 `DonchianTrendFxV1` を実装 (ATR×2 SL + 10-bar trailing Donchian exit)
- `donchian_trend_fx_normal` (allocation 0.50) と `donchian_trend_fx_aggressive` (allocation 0.80) の 2 戦略名で登録、単一 struct を共有
- 4 FX paper account を migration で seed
- `config/default.toml` に USD_JPY と 2 戦略エントリを追加
- FX expected_feeds 登録 (PriceStore 側の配線は既に完了している)

## 非スコープ

- **ペア拡張 (EUR_JPY / GBP_JPY)** — follow-up PR で対応予定 (memory 済み)
- **swing_llm_v1 の有効化** — GEMINI_API_KEY 未設定のまま disable、現行のまま残す
- **OANDA 本番 (live) 口座** — demo のみ、本番接続は将来別 PR
- **Crypto 側の戦略 / 口座 / 価格フィードへの変更** — 完全不変
- **FX 用 executor の新実装** — 既存 `PaperTrader` を pair/exchange-agnostic にそのまま流用
- **OANDA streaming の multi-instrument 最適化** — 今回は 1 ペアのみなので、複数 instrument を 1 接続で束ねる最適化は follow-up

## アーキテクチャ

### データフロー

```
OandaClient ──(streaming /pricing)──> stream task ──raw_tick_tx──> drain task ──> PriceStore
     │                                                                               │
     │                                                                               └──> /api/market/prices
     │                                                                                    /api/health/market-feed
     │
     └──(polling /candles)──> MarketMonitor ──price_tx──> engine task
                                                              ├──> DonchianTrendFxV1 (signal → account sizer)
                                                              ├──> FX position monitor (SL/TP hit)
                                                              └──> crypto position monitor (bitflyer only)
```

**ポイント:**

- `MarketMonitor` (既存 polling) はそのまま残し、M15 candle を engine task に送る
- raw tick は別経路で `OandaClient::stream_prices` から流れ、`PriceStore` に直書き (bitflyer と同じパターン)
- engine task の `price_rx` 受信時に `price_store_for_engine.update()` も呼ぶ (今回の変更で FX も冗長パスで更新される。latest-write-wins なので安全)

### Rust 新規ファイル

#### `crates/strategy/src/donchian_trend_fx.rs`

```rust
pub struct DonchianTrendFxV1 {
    name: String,
    pairs: Vec<Pair>,
    allocation_pct: Decimal,
    history: HashMap<String, VecDeque<Candle>>,
}

impl DonchianTrendFxV1 {
    pub fn new(name: String, pairs: Vec<Pair>, allocation_pct: Decimal) -> Self { ... }
}
```

**定数:**

- `ENTRY_CHANNEL: usize = 20`
- `EXIT_CHANNEL: usize = 10`
- `ATR_PERIOD: usize = 14`
- `ATR_SL_MULT: Decimal = dec!(2.0)`
- `ATR_BASELINE_BARS: usize = 50` (volatility filter baseline)
- `HISTORY_LEN: usize = 200`
- `TIME_LIMIT_HOURS: i64 = 72` (FX トレンドは crypto より長期なので 72h)

**エントリー:**

- **Long**: `prev.close > max(high[−20..−1])` AND `atr(14) > avg(atr[last 50])`
- **Short**: `prev.close < min(low[−20..−1])` AND `atr(14) > avg(atr[last 50])`

**SL (Signal に乗せる):**

- Long: `entry_price - atr × 2`
- Short: `entry_price + atr × 2`

**TP (`on_open_positions` で動的判定):**

- Long: `close < min(low[−10..−1])` → `ExitSignal(StrategyTrailingDonchian)`
- Short: `close > max(high[−10..−1])` → `ExitSignal(StrategyTrailingDonchian)`

- 固定 TP は `Decimal::MAX` / `Decimal::ZERO` 相当の "到達不能値" を Signal に乗せる方針 (crypto donchian の既存パターンを確認して揃える)

**allocation_pct はコンストラクタから注入された値を Signal にそのまま乗せる。**

#### `crates/market/src/oanda.rs` に追加メソッド

```rust
impl OandaClient {
    /// 価格ストリーミングを開始し、受信した各 tick を `tx` に送る。
    /// HTTP long-poll based streaming endpoint なので、
    /// `reqwest::Response::chunk()` を使って JSON 行ごとに parse する。
    /// 接続切断時は呼び出し側 (stream task) が再接続する。
    pub async fn stream_prices(
        &self,
        instruments: &[Pair],
        tx: mpsc::Sender<RawTick>,  // ← bitflyer と同じ (Pair, Decimal, DateTime<Utc>) タプル
    ) -> anyhow::Result<()>;
}
```

**実装詳細:**

- URL: `{base_url}/v3/accounts/{account_id}/pricing/stream?instruments={pair_list}`
- Bearer 認証は既存 `OandaClient` の default headers に設定済み
- レスポンスは 改行区切り JSON (NDJSON)、各行は `{"type": "PRICE", "instrument": "USD_JPY", "time": "...", "bids": [...], "asks": [...]}` か `{"type": "HEARTBEAT", ...}`
- `HEARTBEAT` は 5 秒ごとに来る keepalive メッセージで、price は含まれない
- `PRICE` 受信時: `mid = (bid + ask) / 2` を計算して `RawTick::(pair, mid, ts)` を `tx` に送信
- `HEARTBEAT` 受信時: **前回の `PRICE` の (pair, mid) を覚えておき、heartbeat の timestamp で再送する**。理由: 価格自体は変化していないが、「feed が生きている」ことを PriceStore の 60 秒 freshness に反映させる必要がある。heartbeat を無視すると流動性の低いペア (週末間近の USD/JPY など) で banner が偽赤になる
- 前回 PRICE 未受信 (接続直後) の heartbeat は無視 (送る価格が無い)
- 接続エラー / timeout 時は `anyhow::Error` を返し、呼び出し側で backoff 再接続

**型エイリアス再利用:** `RawTick` は bitflyer と同じ `(Pair, Decimal, DateTime<Utc>)` タプルを使う。現状 `crates/market/src/bitflyer.rs` に `pub type RawTick = ...` として定義されているが、今回 oanda でも使うので **`crates/market/src/lib.rs` に `pub type RawTick = (Pair, Decimal, DateTime<Utc>);` として移動** し、`bitflyer.rs` は `use crate::RawTick;` で参照する。main.rs 側は `auto_trader_market::RawTick` で import。

#### `crates/market/src/monitor.rs` の変更

```rust
pub struct MarketMonitor {
    // 既存フィールド
    raw_tick_tx: Option<mpsc::Sender<RawTick>>,  // ← 追加
}

impl MarketMonitor {
    pub fn with_raw_tick_sink(mut self, tx: mpsc::Sender<RawTick>) -> Self {
        self.raw_tick_tx = Some(tx);
        self
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        // 既存の polling loop に加えて、stream_prices を別 tokio::spawn で起動
        // raw_tick_tx が Some のときだけ stream task を spawn
        if let Some(tx) = self.raw_tick_tx.clone() {
            let client = self.client.clone();  // OandaClient は reqwest::Client 内包なので Clone 可
            let pairs = self.pairs.clone();
            tokio::spawn(async move {
                loop {
                    if let Err(e) = client.stream_prices(&pairs, tx.clone()).await {
                        tracing::warn!("OANDA price stream error (reconnecting): {e}");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            });
        }
        // ... 既存 candle polling loop ...
    }
}
```

`OandaClient` が `Clone` でなければ `Arc<OandaClient>` に変更する。

### Rust 既存ファイル変更

#### `crates/app/src/main.rs`

**1. OANDA 用 raw tick チャネルを新設:**

既存の bitflyer raw_tick_tx 作成付近 (`main.rs:55-60`) で OANDA 用も同様に作る:

```rust
let (bf_raw_tick_tx, mut bf_raw_tick_rx) =
    mpsc::channel::<auto_trader_market::RawTick>(1024);
let (oanda_raw_tick_tx, mut oanda_raw_tick_rx) =
    mpsc::channel::<auto_trader_market::RawTick>(1024);
```

(bitflyer 側の既存変数名は `raw_tick_tx` だが、2 つになるので `bf_` prefix に rename)

**2. FX monitor に `with_raw_tick_sink` を呼ぶ:**

`main.rs:70-93` の fx_monitor 構築部に `.with_raw_tick_sink(oanda_raw_tick_tx.clone())` を追加:

```rust
Some(
    MarketMonitor::new(oanda, fx_pairs, interval_secs, FX_TIMEFRAME, price_tx.clone())
        .with_db(pool.clone())
        .with_raw_tick_sink(oanda_raw_tick_tx.clone())
)
```

**3. drain タスクを 2 本に:**

既存の bitflyer drain タスクに対称な OANDA drain タスクを追加。違いは `Exchange::Oanda` を FeedKey に使うこと:

```rust
let oanda_raw_tick_store = price_store.clone();
let _oanda_raw_tick_drain_handle = tokio::spawn(async move {
    while let Some((pair, price, ts)) = oanda_raw_tick_rx.recv().await {
        oanda_raw_tick_store
            .update(
                crate::price_store::FeedKey::new(
                    auto_trader_core::types::Exchange::Oanda,
                    pair,
                ),
                crate::price_store::LatestTick { price, ts },
            )
            .await;
    }
});
```

**4. FX ポジションモニターを復活:**

現状の `main.rs:582-587` は drain-only:

```rust
// FX position monitor removed: FX paper trading is currently disabled.
// Drain the forwarded FX price channel so senders do not block.
let mut price_monitor_rx = price_monitor_rx;
let pos_monitor_handle = tokio::spawn(async move {
    while price_monitor_rx.recv().await.is_some() {}
});
```

これを crypto_monitor (`main.rs:589〜`) と同じ構造で置き換え。違いは:
- `Exchange::Oanda` でフィルタ
- `crypto_price_tx` → `fx_price_tx` (別チャネル、engine 側の forward も追加)
- `PaperTrader::new(pool, Exchange::Oanda, account_id)`

コード重複が発生するので、**将来の refactor として「ポジションモニタータスクを exchange 引数で汎用化する」** という tech debt を生むが、今回の PR では copy-paste で進める (関連する crypto 側の変更を避け、review diff を小さく保つため)。refactor は follow-up issue に記録。

**5. FX expected_feeds 登録は既に実装済み:**

`main.rs:89-92` に以下が既にある:

```rust
if fx_monitor.is_some() {
    for p in &fx_pairs {
        expected_feeds.push(crate::price_store::FeedKey::new(
            auto_trader_core::types::Exchange::Oanda,
            p.clone(),
        ));
    }
}
```

変更不要。OANDA_API_KEY が設定されて fx_monitor が Some になれば自動で USD_JPY が expected リストに入る。

**6. 戦略登録:**

`main.rs:127 付近` の `match sc.name.as_str()` に新規ブランチ追加:

```rust
name if name.starts_with("donchian_trend_fx") => {
    let allocation_pct = sc.params.get("allocation_pct")
        .and_then(|v| v.as_float())
        .map(|f| Decimal::try_from(f).unwrap_or(dec!(0.5)))
        .unwrap_or(dec!(0.5));
    let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
    let strategy = Box::new(DonchianTrendFxV1::new(
        sc.name.clone(),
        pairs,
        allocation_pct,
    ));
    engine.register(strategy);
}
```

#### `crates/strategy/src/lib.rs`

`pub mod donchian_trend_fx;` を追加。

### 設定 (`config/default.toml`)

```toml
[pairs]
# ... 既存 ...
fx = ["USD_JPY"]  # 新規追加 or active に追加、既存構造を要確認して合わせる

[[strategies]]
name = "donchian_trend_fx_normal"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]
# 通常: allocation 0.50 (証拠金維持率 SL 後 200%、保守)
params = { allocation_pct = 0.50 }

[[strategies]]
name = "donchian_trend_fx_aggressive"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]
# 攻め: allocation 0.80 (証拠金維持率 SL 後 120%、フル寄り)
params = { allocation_pct = 0.80 }
```

既存の `swing_llm_v1` エントリは触らない (disabled で残す)。

### DB Migration

**ファイル:** `migrations/20260409000001_fx_paper_accounts_seed.sql`

```sql
-- FX paper accounts: 2 balances × 2 allocations = 4 accounts.
-- All run donchian_trend_fx on USD/JPY with leverage 25x.
-- UUID prefix b0000000-... to distinguish from crypto (a0000000-...).
INSERT INTO paper_accounts (
    id, name, exchange, initial_balance, current_balance,
    currency, leverage, strategy, account_type, created_at, updated_at
) VALUES
    ('b0000000-0000-0000-0000-000000000010', 'fx_small_normal_v1',
     'oanda', 30000, 30000, 'JPY', 25,
     'donchian_trend_fx_normal', 'paper', NOW(), NOW()),
    ('b0000000-0000-0000-0000-000000000011', 'fx_small_aggressive_v1',
     'oanda', 30000, 30000, 'JPY', 25,
     'donchian_trend_fx_aggressive', 'paper', NOW(), NOW()),
    ('b0000000-0000-0000-0000-000000000012', 'fx_standard_normal_v1',
     'oanda', 100000, 100000, 'JPY', 25,
     'donchian_trend_fx_normal', 'paper', NOW(), NOW()),
    ('b0000000-0000-0000-0000-000000000013', 'fx_standard_aggressive_v1',
     'oanda', 100000, 100000, 'JPY', 25,
     'donchian_trend_fx_aggressive', 'paper', NOW(), NOW())
ON CONFLICT (id) DO NOTHING;
```

`ON CONFLICT DO NOTHING` は既存レコードがある環境での再適用耐性のため。既存 crypto seed と同じパターン。

## エッジケース

- **`OANDA_API_KEY` 未設定**: 既存ロジック通り `FX monitor disabled` で pass、FX 口座は DB にあるが engine に戦略は登録されない (`enabled=true` でもシグナル発火経路が空回り)。起動はクリーン
- **OANDA streaming 再接続中**: drain タスクが raw tick を受け取らない間、候補完成時の `price_rx` 経路で 15 分に 1 回 PriceStore が更新される。60 秒閾値をすぐ割るのでヘルスバナーは赤になる。想定内挙動
- **ATR 計算不能 (history < 50 bars)**: 起動直後 12.5 時間 (50 × 15 分) はエントリー発火しない。warmup loader でこの期間をスキップする工夫は crypto 側にもあるので同じパターンで OK
- **証拠金維持率 100% 割れ (理論上)**: allocation 0.80 + ATR が異常拡大した場合、paper layer では OANDA の実強制決済は再現していないので、paper account は理論上マイナス残高まで行く可能性あり。実運用で観察して allocation を下げる follow-up 判断
- **`HEARTBEAT` のみで価格変動ゼロの時間帯 (週末、主要市場休場)**: price stream は heartbeat を送るが価格は来ない。前回 tick の timestamp が古いままなので 60 秒閾値を割り、ヘルスバナーが赤になる。**これは仕様**: 週末/休場中は実際にトレードできないので「市場フィード停止」の警告が出るのは正しい
- **24/5 市場閉場時 (金曜 NY クローズ 〜 月曜 シドニーオープン)**: 上記と同じ。バナー赤は想定内
- **paper account が 4 口座すべて同じ戦略シグナルで同時エントリー**: 同じ瞬間に 4 トレードが発生する。それぞれの PositionSizer 計算は独立、DB 書き込みも独立。競合なし

## テスト観点

### Rust unit tests

- `DonchianTrendFxV1`:
  - 20-bar ブレイクアウト検出 (prior close > prior high / < prior low)
  - ATR(14) vs avg_ATR(50) フィルタ
  - SL 計算 (`entry ± ATR × 2`、LONG/SHORT で符号正しく)
  - 10-bar trailing Donchian exit 判定 (on_open_positions の戻り値)
  - allocation_pct 注入がコンストラクタから Signal に反映されること
  - history < 50 bars でエントリー拒否
- `OandaClient::stream_prices` は mock HTTP が必要になるので pure テストは避け、URL 構築 / ヘッダー設定の単体テストのみ
- `MarketMonitor::with_raw_tick_sink` のビルダー動作

### 手動スモークテスト

1. `.env` に `OANDA_API_KEY` と `OANDA_ACCOUNT_ID` を追加
2. `docker compose build auto-trader && docker compose up -d auto-trader`
3. 起動ログに `OANDA ... connected` / `FX monitor running` が出る
4. `curl http://localhost:3001/api/health/market-feed` → `oanda USD_JPY healthy`
5. `curl http://localhost:3001/api/market/prices` → OANDA USD_JPY tick が乗っている
6. `curl http://localhost:3001/api/paper-accounts` → 4 FX 口座が見える
7. Positions タブに OANDA exchange が出現
8. 半日〜1 日放置してシグナル発火を確認 (M15 × 20-bar breakout は頻度低、即時には出ない)

## 既存コードへの影響

- `crates/core/src/types.rs`: 変更なし
- `crates/executor/src/paper.rs`: 変更なし (pair/exchange-agnostic)
- `crates/db/src/*`: 変更なし
- `crates/app/src/api/*`: 変更なし (DTO / ルート / handler すべて再利用)
- `dashboard-ui/*`: 変更なし
- `crates/app/src/main.rs`: 変更あり (OANDA drain、FX monitor、戦略登録、raw_tick 配線)
- `crates/market/src/oanda.rs`: stream_prices メソッド追加
- `crates/market/src/monitor.rs`: with_raw_tick_sink 追加
- `crates/market/src/lib.rs`: `RawTick` 型を pub 移動 (or bitflyer から re-export)
- `crates/market/src/bitflyer.rs`: 必要なら `RawTick` を lib から参照するよう変更
- `crates/strategy/src/lib.rs`: `pub mod donchian_trend_fx;`
- `crates/strategy/src/donchian_trend_fx.rs`: 新規
- `config/default.toml`: 2 戦略エントリ追加、fx pair エントリ追加
- `migrations/20260409000001_fx_paper_accounts_seed.sql`: 新規

## 将来の拡張余地 (非スコープだが記録)

- **ペア拡張 (EUR_JPY, GBP_JPY)**: follow-up PR、memory に記録済み
- **swing_llm_v1 有効化**: GEMINI_API_KEY 入手後
- **ポジションモニタータスクの DRY 化**: crypto/fx で copy-paste された構造を `run_position_monitor(exchange, pool, price_rx)` ヘルパに統合
- **FX 用追加戦略**: momentum / carry / mean-reversion 等、単一 struct で多 instance パターンが確立すれば追加しやすい
- **多 instrument streaming の最適化**: OANDA pricing stream は 1 接続で複数 instrument を束ねられる (`instruments=USD_JPY,EUR_JPY,GBP_JPY`)。ペア拡張 PR で対応
- **証拠金維持率の paper 再現**: 実際の強制決済挙動を paper 層でシミュレート (現状 OANDA デモに任せている)
