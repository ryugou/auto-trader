# Crypto Paper Trading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** bitFlyer Crypto CFD（BTC/JPY）のペーパートレード機能を既存 FX パイプラインに統合し、複数口座での並行シミュレーションを実現する。

**Architecture:** 既存の crate 構造にそのまま暗号資産を組み込む。core に Exchange enum を追加し、PriceEvent/Trade/Candle に exchange フィールドを追加。bitFlyer WebSocket で Ticker を受信し CandleBuilder で自前ローソク足を構築。複数 PaperAccount がリスク額ベースのポジションサイジングで独立運用。オーバーナイト手数料を日次シミュレーション。

**Tech Stack:** Rust, tokio, tokio-tungstenite (WebSocket), serde_json, sqlx (PostgreSQL), rust_decimal

**Spec:** `specs/crypto-paper-trading.md`

**Existing codebase reference:** feature/phase0 ブランチ（main にマージ済み）

---

## File Structure

### 新規作成

| ファイル | 責務 |
|---------|------|
| `crates/market/src/bitflyer.rs` | bitFlyer Lightning WebSocket/REST クライアント |
| `crates/market/src/candle_builder.rs` | Ticker データから OHLCV ローソク足を構築 |
| `crates/market/src/provider.rs` | MarketDataProvider トレイト定義 |
| `crates/strategy/src/crypto_trend.rs` | 暗号資産向けトレンドフォロー戦略 |
| `crates/executor/src/position_sizer.rs` | リスク額ベースのポジションサイジング |
| `migrations/20260405000001_crypto_paper_trading.sql` | exchange カラム追加、paper_accounts テーブル等 |

### 変更

| ファイル | 変更内容 |
|---------|---------|
| `crates/core/src/types.rs` | Exchange enum 追加、Trade/Candle/PriceEvent にフィールド追加、volume 型変更 |
| `crates/core/src/event.rs` | PriceEvent に exchange フィールド追加 |
| `crates/core/src/config.rs` | BitflyerConfig, PairConfig, PaperAccountConfig, PositionSizingConfig 追加 |
| `crates/core/src/executor.rs` | OrderExecutor に account context を渡せるよう拡張 |
| `crates/market/src/lib.rs` | 新モジュール公開 |
| `crates/market/src/oanda.rs` | MarketDataProvider 実装に変換 |
| `crates/market/src/monitor.rs` | exchange フィールド対応 |
| `crates/market/Cargo.toml` | tokio-tungstenite 依存追加 |
| `crates/strategy/src/trend_follow.rs` | pip サイズ計算を PairConfig から取得 |
| `crates/strategy/src/lib.rs` | crypto_trend モジュール公開 |
| `crates/executor/src/paper.rs` | quantity/fees/paper_account_id 対応、PositionSizer 統合 |
| `crates/executor/src/lib.rs` | position_sizer モジュール公開 |
| `crates/db/src/candles.rs` | exchange カラム対応 |
| `crates/db/src/trades.rs` | quantity/leverage/fees/paper_account_id/exchange 対応 |
| `crates/db/src/summary.rs` | exchange/paper_account_id 対応 |
| `crates/app/src/main.rs` | bitFlyer 監視タスク、複数 PaperAccount ワイヤリング、オーバーナイト手数料タスク |
| `crates/app/Cargo.toml` | 依存追加 |
| `config/default.toml` | bitflyer/crypto 設定追加 |
| `Cargo.toml` | tokio-tungstenite を workspace deps に追加 |

---

## Task 1: Exchange enum と core 型の拡張

**Files:**
- Modify: `crates/core/src/types.rs`
- Modify: `crates/core/src/event.rs`
- Test: `crates/core/src/types.rs` (既存テスト内)

- [ ] **Step 1: Exchange enum のテストを追加**

`crates/core/src/types.rs` の `mod tests` に追加:

```rust
#[test]
fn exchange_serializes_snake_case() {
    let json = serde_json::to_string(&Exchange::BitflyerCfd).unwrap();
    assert_eq!(json, r#""bitflyer_cfd""#);
    let json = serde_json::to_string(&Exchange::Oanda).unwrap();
    assert_eq!(json, r#""oanda""#);
}

#[test]
fn exchange_display() {
    assert_eq!(Exchange::Oanda.as_str(), "oanda");
    assert_eq!(Exchange::BitflyerCfd.as_str(), "bitflyer_cfd");
}
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cargo test -p auto-trader-core exchange_serializes`
Expected: FAIL — `Exchange` が未定義

- [ ] **Step 3: Exchange enum を実装**

`crates/core/src/types.rs` の `Direction` enum の前に追加:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exchange {
    Oanda,
    BitflyerCfd,
}

impl Exchange {
    pub fn as_str(&self) -> &'static str {
        match self {
            Exchange::Oanda => "oanda",
            Exchange::BitflyerCfd => "bitflyer_cfd",
        }
    }
}

impl std::fmt::Display for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p auto-trader-core exchange_`
Expected: PASS

- [ ] **Step 5: Candle, Trade, PriceEvent に exchange フィールドを追加**

`crates/core/src/types.rs` — `Candle` struct:
```rust
pub struct Candle {
    pub pair: Pair,
    pub exchange: Exchange,
    pub timeframe: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Option<u64>,  // i32 → u64 に変更
    pub timestamp: DateTime<Utc>,
}
```

`Trade` struct に追加:
```rust
pub struct Trade {
    pub id: Uuid,
    pub strategy_name: String,
    pub pair: Pair,
    pub exchange: Exchange,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub quantity: Option<Decimal>,
    pub leverage: Decimal,
    pub fees: Decimal,
    pub paper_account_id: Option<Uuid>,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_pips: Option<Decimal>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<ExitReason>,
    pub mode: TradeMode,
    pub status: TradeStatus,
}
```

`crates/core/src/event.rs` — `PriceEvent`:
```rust
#[derive(Debug, Clone)]
pub struct PriceEvent {
    pub pair: Pair,
    pub exchange: Exchange,
    pub candle: Candle,
    pub indicators: HashMap<String, Decimal>,
    pub timestamp: DateTime<Utc>,
}
```

- [ ] **Step 6: 既存テストとコンパイルエラーを全て修正**

Exchange フィールドの追加により、Candle/Trade/PriceEvent の構築箇所が全てコンパイルエラーになる。以下のパターンで修正:

- `Candle { ... }` → `exchange: Exchange::Oanda` を追加（FX 既存コード）
- `Trade { ... }` → `exchange: Exchange::Oanda, quantity: None, leverage: Decimal::ONE, fees: Decimal::ZERO, paper_account_id: None` を追加
- `PriceEvent { ... }` → `exchange: Exchange::Oanda` を追加
- `volume: Some(100)` → `volume: Some(100u64)` に変更（i32 → u64）

修正対象ファイル（全て `Exchange::Oanda` をデフォルトとして追加）:
- `crates/executor/src/paper.rs` — Trade 構築
- `crates/market/src/oanda.rs` — Candle 構築
- `crates/market/src/monitor.rs` — PriceEvent 構築
- `crates/strategy/src/trend_follow.rs` — テスト内の PriceEvent/Candle
- `crates/strategy/src/engine.rs` — テスト内
- `crates/backtest/src/runner.rs` — PriceEvent 構築
- `crates/app/tests/integration_test.rs` — テスト内
- `crates/app/src/main.rs` — Vegapunk ingest 内の Trade clone
- `crates/db/src/candles.rs` — CandleRow の volume 型を `Option<i64>` に変更（PostgreSQL INTEGER → Rust u64 は i64 経由）
- `crates/db/src/trades.rs` — TradeRow に新フィールド追加

- [ ] **Step 7: cargo test で全テスト通過を確認**

Run: `cargo test`
Expected: 全24テスト PASS

- [ ] **Step 8: コミット**

```bash
git add -A
git commit -m "feat(core): add Exchange enum, extend Trade/Candle/PriceEvent with exchange and trade fields"
```

---

## Task 2: 設定ファイルの拡張

**Files:**
- Modify: `crates/core/src/config.rs`
- Modify: `config/default.toml`
- Test: `crates/core/src/config.rs` (既存テスト更新)

- [ ] **Step 1: 設定テストを更新**

`crates/core/src/config.rs` の `parse_minimal_config` テストに bitflyer/pair_config/paper_accounts を追加:

```rust
#[test]
fn parse_config_with_crypto() {
    let toml_str = r#"
[oanda]
api_url = "https://api-fxpractice.oanda.com"
account_id = "101-001-12345678-001"

[bitflyer]
ws_url = "wss://ws.lightstream.bitflyer.com/json-rpc"
api_url = "https://api.bitflyer.com"

[vegapunk]
endpoint = "http://localhost:3000"
schema = "fx-trading"

[database]
url = "postgresql://user:pass@localhost:5432/auto_trader"

[monitor]
interval_secs = 60

[pairs]
fx = ["USD_JPY"]
crypto = ["FX_BTC_JPY"]

[pair_config.FX_BTC_JPY]
price_unit = 1
min_order_size = 0.001

[pair_config.USD_JPY]
price_unit = 0.001
min_order_size = 1

[position_sizing]
method = "risk_based"
risk_rate = 0.02

[[strategies]]
name = "trend_follow_v1"
enabled = true
mode = "paper"
pairs = ["USD_JPY"]

[[paper_accounts]]
name = "crypto_real"
exchange = "bitflyer_cfd"
initial_balance = 5233
leverage = 2
currency = "JPY"
"#;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.bitflyer.as_ref().unwrap().ws_url, "wss://ws.lightstream.bitflyer.com/json-rpc");
    assert_eq!(config.pairs.crypto.as_ref().unwrap().len(), 1);
    assert_eq!(config.pair_config.get("FX_BTC_JPY").unwrap().price_unit.to_string(), "1");
    assert_eq!(config.paper_accounts.len(), 1);
    assert_eq!(config.paper_accounts[0].name, "crypto_real");
    assert_eq!(config.paper_accounts[0].leverage.to_string(), "2");
    assert_eq!(config.position_sizing.as_ref().unwrap().risk_rate.to_string(), "0.02");
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `cargo test -p auto-trader-core parse_config_with_crypto`
Expected: FAIL — 新しいフィールドが未定義

- [ ] **Step 3: 設定構造体を追加**

`crates/core/src/config.rs` に追加:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct BitflyerConfig {
    pub ws_url: String,
    pub api_url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PairConfig {
    pub price_unit: Decimal,
    pub min_order_size: Decimal,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PaperAccountConfig {
    pub name: String,
    pub exchange: String,
    pub initial_balance: Decimal,
    pub leverage: Decimal,
    #[serde(default = "default_currency")]
    pub currency: String,
}

fn default_currency() -> String {
    "JPY".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct PositionSizingConfig {
    pub method: String,
    pub risk_rate: Decimal,
}
```

`use rust_decimal::Decimal;` を imports に追加。

`AppConfig` に追加:
```rust
pub struct AppConfig {
    pub oanda: OandaConfig,
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
    pub paper_accounts: Vec<PaperAccountConfig>,
    #[serde(default)]
    pub macro_analyst: Option<MacroAnalystConfig>,
    #[serde(default)]
    pub gemini: Option<GeminiConfig>,
}
```

`PairsConfig` を変更:
```rust
pub struct PairsConfig {
    #[serde(default)]
    pub fx: Vec<String>,
    #[serde(default)]
    pub crypto: Option<Vec<String>>,
    // 後方互換: 旧 active フィールドが存在する場合は fx として扱う
    #[serde(default)]
    pub active: Vec<String>,
}
```

- [ ] **Step 4: テスト通過を確認**

Run: `cargo test -p auto-trader-core parse_config`
Expected: PASS（新旧両方のテスト）

- [ ] **Step 5: config/default.toml を更新**

既存 `[pairs]` セクション以降に追記:

```toml
[pairs]
fx = ["USD_JPY", "EUR_USD"]
crypto = ["FX_BTC_JPY"]

[bitflyer]
ws_url = "wss://ws.lightstream.bitflyer.com/json-rpc"
api_url = "https://api.bitflyer.com"

[pair_config.FX_BTC_JPY]
price_unit = 1
min_order_size = 0.001

[pair_config.USD_JPY]
price_unit = 0.001
min_order_size = 1

[pair_config.EUR_USD]
price_unit = 0.00001
min_order_size = 1

[position_sizing]
method = "risk_based"
risk_rate = 0.02

[[paper_accounts]]
name = "crypto_real"
exchange = "bitflyer_cfd"
initial_balance = 5233
leverage = 2
currency = "JPY"

[[paper_accounts]]
name = "crypto_100k"
exchange = "bitflyer_cfd"
initial_balance = 100000
leverage = 2
currency = "JPY"
```

- [ ] **Step 6: main.rs の pairs 参照を更新**

`crates/app/src/main.rs` で `config.pairs.active` を使っている箇所を `config.pairs.fx` に変更。`active` が空でなければ `active` を使うフォールバック付き:

```rust
let fx_pairs: Vec<Pair> = if !config.pairs.active.is_empty() {
    config.pairs.active.iter().map(|s| Pair::new(s)).collect()
} else {
    config.pairs.fx.iter().map(|s| Pair::new(s)).collect()
};
```

- [ ] **Step 7: cargo test で全テスト通過を確認**

Run: `cargo test`
Expected: 全テスト PASS

- [ ] **Step 8: コミット**

```bash
git add -A
git commit -m "feat(config): add bitflyer, pair_config, paper_accounts, position_sizing settings"
```

---

## Task 3: DB マイグレーション

**Files:**
- Create: `migrations/20260405000001_crypto_paper_trading.sql`
- Modify: `crates/db/src/candles.rs`
- Modify: `crates/db/src/trades.rs`
- Modify: `crates/db/src/summary.rs`

- [ ] **Step 1: マイグレーション SQL を作成**

```sql
-- paper_accounts テーブル（trades より先に作成 — FK 参照のため）
CREATE TABLE paper_accounts (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    exchange TEXT NOT NULL,
    initial_balance DECIMAL NOT NULL,
    current_balance DECIMAL NOT NULL,
    currency TEXT NOT NULL DEFAULT 'JPY',
    leverage DECIMAL NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- trades: exchange, quantity, leverage, fees, paper_account_id
ALTER TABLE trades ADD COLUMN exchange TEXT NOT NULL DEFAULT 'oanda';
ALTER TABLE trades ADD COLUMN quantity DECIMAL;
ALTER TABLE trades ADD COLUMN leverage DECIMAL NOT NULL DEFAULT 1;
ALTER TABLE trades ADD COLUMN fees DECIMAL NOT NULL DEFAULT 0;
ALTER TABLE trades ADD COLUMN paper_account_id UUID REFERENCES paper_accounts(id);
CREATE INDEX idx_trades_exchange ON trades (exchange);

-- price_candles: exchange + UNIQUE 制約更新
ALTER TABLE price_candles ADD COLUMN exchange TEXT NOT NULL DEFAULT 'oanda';
ALTER TABLE price_candles DROP CONSTRAINT price_candles_pair_timeframe_timestamp_key;
ALTER TABLE price_candles ADD CONSTRAINT price_candles_exchange_pair_tf_ts_key
    UNIQUE (exchange, pair, timeframe, timestamp);

-- daily_summary: exchange, paper_account_id
ALTER TABLE daily_summary ADD COLUMN exchange TEXT NOT NULL DEFAULT 'oanda';
ALTER TABLE daily_summary ADD COLUMN paper_account_id UUID REFERENCES paper_accounts(id);
ALTER TABLE daily_summary DROP CONSTRAINT daily_summary_date_strategy_name_pair_mode_key;
ALTER TABLE daily_summary ADD CONSTRAINT daily_summary_unique_key
    UNIQUE (date, strategy_name, pair, mode, exchange, paper_account_id);
CREATE UNIQUE INDEX daily_summary_fx_unique
    ON daily_summary (date, strategy_name, pair, mode, exchange)
    WHERE paper_account_id IS NULL;
```

- [ ] **Step 2: candles.rs を exchange 対応に更新**

`upsert_candle` — INSERT/CONFLICT に exchange を追加:
```rust
pub async fn upsert_candle(pool: &PgPool, candle: &Candle) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO price_candles (exchange, pair, timeframe, open, high, low, close, volume, timestamp)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           ON CONFLICT (exchange, pair, timeframe, timestamp) DO UPDATE
           SET open = $4, high = $5, low = $6, close = $7, volume = $8"#,
    )
    .bind(candle.exchange.as_str())
    .bind(&candle.pair.0)
    .bind(&candle.timeframe)
    .bind(candle.open)
    .bind(candle.high)
    .bind(candle.low)
    .bind(candle.close)
    .bind(candle.volume.map(|v| v as i64))
    .bind(candle.timestamp)
    .execute(pool)
    .await?;
    Ok(())
}
```

`get_candles` — WHERE に exchange を追加。`CandleRow` の `volume` を `Option<i64>` に変更し、`From` 実装で `u64` に変換。exchange フィールドを追加。

- [ ] **Step 3: trades.rs を拡張フィールド対応に更新**

`insert_trade` — exchange, quantity, leverage, fees, paper_account_id を INSERT に追加。
`update_trade_closed` — fees の累積を考慮（fees は close 時に最終値を渡す）。
`TradeRow` — 新フィールド追加、`TryFrom` 実装更新。

- [ ] **Step 4: summary.rs を exchange/paper_account_id 対応に更新**

`upsert_daily_summary` — exchange, paper_account_id パラメータ追加。
`update_daily_max_drawdown` — exchange フィルタ追加。

- [ ] **Step 5: cargo check で全クレートのコンパイルを確認**

Run: `cargo check`
Expected: 成功

- [ ] **Step 6: コミット**

```bash
git add -A
git commit -m "feat(db): add crypto paper trading migration and update queries"
```

---

## Task 4: MarketDataProvider トレイトと OandaClient の適合

**Files:**
- Create: `crates/market/src/provider.rs`
- Modify: `crates/market/src/oanda.rs`
- Modify: `crates/market/src/lib.rs`

- [ ] **Step 1: MarketDataProvider トレイトを定義**

`crates/market/src/provider.rs`:
```rust
use auto_trader_core::types::{Candle, Pair};
use rust_decimal::Decimal;

#[async_trait::async_trait]
pub trait MarketDataProvider: Send + Sync {
    async fn get_candles(&self, pair: &Pair, timeframe: &str, count: u32) -> anyhow::Result<Vec<Candle>>;
    async fn get_latest_price(&self, pair: &Pair) -> anyhow::Result<Decimal>;
}
```

- [ ] **Step 2: OandaClient に MarketDataProvider を実装**

`crates/market/src/oanda.rs` に追加:
```rust
#[async_trait::async_trait]
impl crate::provider::MarketDataProvider for OandaClient {
    async fn get_candles(&self, pair: &Pair, timeframe: &str, count: u32) -> anyhow::Result<Vec<Candle>> {
        self.get_candles(pair, timeframe, count).await
    }
    async fn get_latest_price(&self, pair: &Pair) -> anyhow::Result<Decimal> {
        self.get_latest_price(pair).await
    }
}
```

注: inherent method と trait method が名前衝突するので、inherent method をそのまま呼ぶ形で実装。OandaClient の既存 API は変更しない。

- [ ] **Step 3: lib.rs にモジュール公開を追加**

```rust
pub mod provider;
```

- [ ] **Step 4: cargo check を確認**

Run: `cargo check -p auto-trader-market`
Expected: 成功

- [ ] **Step 5: コミット**

```bash
git add -A
git commit -m "feat(market): add MarketDataProvider trait and implement for OandaClient"
```

---

## Task 5: CandleBuilder（Ticker → OHLCV）

**Files:**
- Create: `crates/market/src/candle_builder.rs`
- Test: `crates/market/src/candle_builder.rs` (モジュール内テスト)

- [ ] **Step 1: CandleBuilder のテストを書く**

`crates/market/src/candle_builder.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use chrono::TimeZone;

    #[test]
    fn builds_candle_from_ticks() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut builder = CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, "M1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();

        builder.on_tick(dec!(15000000), dec!(0.1), base);
        builder.on_tick(dec!(15100000), dec!(0.2), base + chrono::Duration::seconds(10));
        builder.on_tick(dec!(14900000), dec!(0.15), base + chrono::Duration::seconds(30));
        builder.on_tick(dec!(15050000), dec!(0.05), base + chrono::Duration::seconds(50));

        // Minute hasn't ended yet — no candle emitted
        assert!(builder.try_complete(base + chrono::Duration::seconds(50)).is_none());

        // Minute ends
        let candle = builder.try_complete(base + chrono::Duration::seconds(61)).unwrap();
        assert_eq!(candle.open, dec!(15000000));
        assert_eq!(candle.high, dec!(15100000));
        assert_eq!(candle.low, dec!(14900000));
        assert_eq!(candle.close, dec!(15050000));
        assert_eq!(candle.volume, Some(50)); // 0.1 + 0.2 + 0.15 + 0.05 = 0.5 → satoshi scale depends on impl
        assert_eq!(candle.exchange, Exchange::BitflyerCfd);
    }

    #[test]
    fn empty_period_returns_none() {
        let pair = Pair::new("FX_BTC_JPY");
        let mut builder = CandleBuilder::new(pair, Exchange::BitflyerCfd, "M1".to_string());
        let base = Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap();
        assert!(builder.try_complete(base + chrono::Duration::seconds(61)).is_none());
    }
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `cargo test -p auto-trader-market candle_builder`
Expected: FAIL — CandleBuilder が未定義

- [ ] **Step 3: CandleBuilder を実装**

```rust
use auto_trader_core::types::{Candle, Exchange, Pair};
use chrono::{DateTime, Utc, Timelike};
use rust_decimal::Decimal;

pub struct CandleBuilder {
    pair: Pair,
    exchange: Exchange,
    timeframe: String,
    period_secs: u64,
    current_period_start: Option<DateTime<Utc>>,
    open: Option<Decimal>,
    high: Option<Decimal>,
    low: Option<Decimal>,
    close: Option<Decimal>,
    volume: Decimal,
}

impl CandleBuilder {
    pub fn new(pair: Pair, exchange: Exchange, timeframe: String) -> Self {
        let period_secs = match timeframe.as_str() {
            "M1" => 60,
            "M5" => 300,
            "H1" => 3600,
            other => panic!("unsupported timeframe: {other}"),
        };
        Self {
            pair, exchange, timeframe, period_secs,
            current_period_start: None,
            open: None, high: None, low: None, close: None,
            volume: Decimal::ZERO,
        }
    }

    fn period_start(&self, ts: DateTime<Utc>) -> DateTime<Utc> {
        let secs = ts.timestamp();
        let period = self.period_secs as i64;
        let truncated = secs - (secs % period);
        DateTime::from_timestamp(truncated, 0).unwrap()
    }

    pub fn on_tick(&mut self, price: Decimal, size: Decimal, ts: DateTime<Utc>) {
        let ps = self.period_start(ts);
        if self.current_period_start != Some(ps) {
            // New period — reset (any previous incomplete candle is discarded on period change via try_complete)
            if self.current_period_start.is_some() && self.open.is_some() {
                // Caller should have called try_complete before this new period
            }
            self.current_period_start = Some(ps);
            self.open = Some(price);
            self.high = Some(price);
            self.low = Some(price);
            self.close = Some(price);
            self.volume = size;
        } else {
            if price > self.high.unwrap() { self.high = Some(price); }
            if price < self.low.unwrap() { self.low = Some(price); }
            self.close = Some(price);
            self.volume += size;
        }
    }

    /// Returns a completed candle if the given timestamp is past the current period.
    pub fn try_complete(&mut self, now: DateTime<Utc>) -> Option<Candle> {
        let ps = self.current_period_start?;
        let period_end = ps + chrono::Duration::seconds(self.period_secs as i64);
        if now < period_end {
            return None;
        }
        let candle = Candle {
            pair: self.pair.clone(),
            exchange: self.exchange,
            timeframe: self.timeframe.clone(),
            open: self.open.take()?,
            high: self.high.take()?,
            low: self.low.take()?,
            close: self.close.take()?,
            volume: Some(self.volume.to_string().parse::<f64>().ok()?.round() as u64),
            timestamp: ps,
        };
        self.current_period_start = None;
        self.volume = Decimal::ZERO;
        Some(candle)
    }
}
```

注: volume の変換は実装時に bitFlyer の volume 定義を確認して調整すること。上記は概念実装。

- [ ] **Step 4: テスト通過を確認**

Run: `cargo test -p auto-trader-market candle_builder`
Expected: PASS（volume のアサーション値は実装に合わせて調整）

- [ ] **Step 5: lib.rs にモジュール公開を追加**

```rust
pub mod candle_builder;
```

- [ ] **Step 6: コミット**

```bash
git add -A
git commit -m "feat(market): add CandleBuilder for ticker-to-OHLCV conversion"
```

---

## Task 6: bitFlyer WebSocket クライアント

**Files:**
- Create: `crates/market/src/bitflyer.rs`
- Modify: `crates/market/src/lib.rs`
- Modify: `crates/market/Cargo.toml`
- Modify: `Cargo.toml` (workspace deps)

- [ ] **Step 1: tokio-tungstenite を workspace 依存に追加**

`Cargo.toml` (ルート workspace):
```toml
tokio-tungstenite = { version = "0.24", features = ["native-tls"] }
futures-util = "0.3"
```

`crates/market/Cargo.toml`:
```toml
tokio-tungstenite = { workspace = true }
futures-util = { workspace = true }
```

- [ ] **Step 2: bitFlyer クライアントを実装**

`crates/market/src/bitflyer.rs`:

```rust
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use crate::candle_builder::CandleBuilder;
use crate::indicators;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use std::str::FromStr;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[derive(serde::Deserialize)]
struct JsonRpcMessage {
    method: Option<String>,
    params: Option<TickerParams>,
}

#[derive(serde::Deserialize)]
struct TickerParams {
    message: TickerMessage,
}

#[derive(serde::Deserialize)]
struct TickerMessage {
    product_code: String,
    best_bid: f64,
    best_ask: f64,
    ltp: f64,
    volume: f64,
    timestamp: String,
}

pub struct BitflyerMonitor {
    ws_url: String,
    pairs: Vec<Pair>,
    timeframe: String,
    tx: mpsc::Sender<PriceEvent>,
    pool: Option<PgPool>,
}

impl BitflyerMonitor {
    pub fn new(
        ws_url: &str,
        pairs: Vec<Pair>,
        timeframe: &str,
        tx: mpsc::Sender<PriceEvent>,
    ) -> Self {
        Self {
            ws_url: ws_url.to_string(),
            pairs,
            timeframe: timeframe.to_string(),
            tx,
            pool: None,
        }
    }

    pub fn with_db(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let mut builders: HashMap<String, CandleBuilder> = HashMap::new();
        for pair in &self.pairs {
            builders.insert(
                pair.0.clone(),
                CandleBuilder::new(pair.clone(), Exchange::BitflyerCfd, self.timeframe.clone()),
            );
        }
        // Store accumulated closes for indicator calculation
        let mut closes_map: HashMap<String, Vec<Decimal>> = HashMap::new();

        loop {
            match self.connect_and_stream(&mut builders, &mut closes_map).await {
                Ok(()) => {
                    tracing::info!("bitflyer websocket closed normally");
                    break;
                }
                Err(e) => {
                    if self.tx.is_closed() {
                        tracing::info!("price channel closed, stopping bitflyer monitor");
                        return Ok(());
                    }
                    tracing::warn!("bitflyer websocket error, reconnecting in 5s: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
        Ok(())
    }

    async fn connect_and_stream(
        &self,
        builders: &mut HashMap<String, CandleBuilder>,
        closes_map: &mut HashMap<String, Vec<Decimal>>,
    ) -> anyhow::Result<()> {
        let (ws, _) = connect_async(&self.ws_url).await?;
        let (mut write, mut read) = ws.split();
        tracing::info!("bitflyer websocket connected: {}", self.ws_url);

        // Subscribe to ticker channels
        for pair in &self.pairs {
            let subscribe = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "subscribe",
                "params": { "channel": format!("lightning_ticker_{}", pair.0) }
            });
            write.send(Message::Text(subscribe.to_string())).await?;
        }

        while let Some(msg) = read.next().await {
            let msg = msg?;
            let Message::Text(text) = msg else { continue };

            let rpc: JsonRpcMessage = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(_) => continue,
            };

            if rpc.method.as_deref() != Some("channelMessage") {
                continue;
            }

            let Some(params) = rpc.params else { continue };
            let ticker = params.message;
            let product_code = &ticker.product_code;

            let Some(builder) = builders.get_mut(product_code) else { continue };

            let price = Decimal::from_str(&format!("{}", ticker.ltp))?;
            let size = Decimal::from_str(&format!("{}", ticker.volume))?;
            let ts = chrono::DateTime::parse_from_rfc3339(&ticker.timestamp)?.with_timezone(&chrono::Utc);

            builder.on_tick(price, size, ts);

            if let Some(candle) = builder.try_complete(ts) {
                // Save candle to DB
                if let Some(pool) = &self.pool {
                    if let Err(e) = auto_trader_db::candles::upsert_candle(pool, &candle).await {
                        tracing::warn!("failed to save crypto candle: {e}");
                    }
                }

                // Accumulate closes for indicators
                let closes = closes_map.entry(product_code.clone()).or_default();
                closes.push(candle.close);
                if closes.len() > 200 { closes.drain(..closes.len() - 200); }

                let mut indicator_map = HashMap::new();
                if let Some(v) = indicators::sma(closes, 20) {
                    indicator_map.insert("sma_20".to_string(), v);
                }
                if let Some(v) = indicators::sma(closes, 50) {
                    indicator_map.insert("sma_50".to_string(), v);
                }
                if let Some(v) = indicators::rsi(closes, 14) {
                    indicator_map.insert("rsi_14".to_string(), v);
                }

                let event = PriceEvent {
                    pair: candle.pair.clone(),
                    exchange: Exchange::BitflyerCfd,
                    timestamp: candle.timestamp,
                    candle,
                    indicators: indicator_map,
                };

                if self.tx.send(event).await.is_err() {
                    tracing::info!("price channel closed, stopping bitflyer monitor");
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 3: lib.rs にモジュール公開を追加**

```rust
pub mod bitflyer;
```

- [ ] **Step 4: cargo check を確認**

Run: `cargo check -p auto-trader-market`
Expected: 成功

- [ ] **Step 5: コミット**

```bash
git add -A
git commit -m "feat(market): add bitFlyer WebSocket client with auto-reconnect"
```

---

## Task 7: PositionSizer

**Files:**
- Create: `crates/executor/src/position_sizer.rs`
- Modify: `crates/executor/src/lib.rs`
- Test: `crates/executor/src/position_sizer.rs` (モジュール内テスト)

- [ ] **Step 1: PositionSizer のテストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::Pair;
    use rust_decimal_macros::dec;

    #[test]
    fn calculates_quantity_from_risk() {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
        let sizer = PositionSizer::new(dec!(0.02), min_sizes);

        // balance=100000, risk_rate=2% → max_loss=2000
        // SL距離=200000円 → quantity = 2000/200000 = 0.01 BTC
        let qty = sizer.calculate_quantity(
            &Pair::new("FX_BTC_JPY"), dec!(100000), dec!(15000000), dec!(14800000), dec!(2),
        );
        assert_eq!(qty, Some(dec!(0.01)));
    }

    #[test]
    fn rejects_below_min_order_size() {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
        let sizer = PositionSizer::new(dec!(0.02), min_sizes);

        // balance=1000, risk_rate=2% → max_loss=20
        // SL距離=200000 → quantity = 20/200000 = 0.0001 < min 0.001
        let qty = sizer.calculate_quantity(
            &Pair::new("FX_BTC_JPY"), dec!(1000), dec!(15000000), dec!(14800000), dec!(2),
        );
        assert_eq!(qty, None);
    }

    #[test]
    fn rejects_exceeds_margin() {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
        let sizer = PositionSizer::new(dec!(0.50), min_sizes); // 50% risk

        // balance=5233, risk_rate=50% → max_loss=2616.5
        // SL距離=100000 → quantity = 2616.5/100000 = 0.026165
        // margin_required = 0.026165 * 15000000 / 2 = 196237.5 > 5233 → reject
        let qty = sizer.calculate_quantity(
            &Pair::new("FX_BTC_JPY"), dec!(5233), dec!(15000000), dec!(14900000), dec!(2),
        );
        assert_eq!(qty, None);
    }
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `cargo test -p auto-trader-executor position_sizer`
Expected: FAIL

- [ ] **Step 3: PositionSizer を実装**

```rust
use auto_trader_core::types::Pair;
use rust_decimal::Decimal;
use std::collections::HashMap;

pub struct PositionSizer {
    risk_rate: Decimal,
    min_order_sizes: HashMap<Pair, Decimal>,
}

impl PositionSizer {
    pub fn new(risk_rate: Decimal, min_order_sizes: HashMap<Pair, Decimal>) -> Self {
        Self { risk_rate, min_order_sizes }
    }

    /// Returns the position quantity, or None if the trade should be skipped
    /// (below min order size or exceeds margin).
    pub fn calculate_quantity(
        &self,
        pair: &Pair,
        balance: Decimal,
        entry_price: Decimal,
        stop_loss: Decimal,
        leverage: Decimal,
    ) -> Option<Decimal> {
        let max_loss = balance * self.risk_rate;
        let sl_distance = (entry_price - stop_loss).abs();
        if sl_distance == Decimal::ZERO {
            return None;
        }

        let quantity = max_loss / sl_distance;

        // Check minimum order size
        let min_size = self.min_order_sizes.get(pair).copied().unwrap_or(Decimal::ZERO);
        if quantity < min_size {
            return None;
        }

        // Check margin requirement
        let margin_required = quantity * entry_price / leverage;
        if margin_required > balance {
            return None;
        }

        // Truncate to min_size precision
        if min_size > Decimal::ZERO {
            let truncated = (quantity / min_size).floor() * min_size;
            if truncated < min_size {
                return None;
            }
            Some(truncated)
        } else {
            Some(quantity)
        }
    }
}
```

- [ ] **Step 4: テスト通過を確認**

Run: `cargo test -p auto-trader-executor position_sizer`
Expected: PASS

- [ ] **Step 5: lib.rs にモジュール公開を追加**

```rust
pub mod position_sizer;
```

- [ ] **Step 6: コミット**

```bash
git add -A
git commit -m "feat(executor): add PositionSizer with risk-based quantity calculation"
```

---

## Task 8: PaperTrader の拡張（quantity/fees/paper_account_id）

**Files:**
- Modify: `crates/executor/src/paper.rs`
- Test: `crates/executor/src/paper.rs` (既存テスト更新 + 新テスト)

- [ ] **Step 1: 暗号資産のテストを追加**

```rust
#[tokio::test]
async fn crypto_position_with_quantity() {
    let trader = PaperTrader::new(dec!(100000), dec!(2), Some(Uuid::new_v4()));
    let signal = Signal {
        strategy_name: "crypto_trend_v1".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        entry_price: dec!(15000000),
        stop_loss: dec!(14800000),
        take_profit: dec!(15400000),
        confidence: 0.8,
        timestamp: Utc::now(),
    };
    let trade = trader.execute_with_quantity(&signal, dec!(0.01)).await.unwrap();
    assert_eq!(trade.quantity, Some(dec!(0.01)));
    assert_eq!(trade.leverage, dec!(2));
    assert_eq!(trade.paper_account_id, trader.account_id());

    // Close: pnl = (15400000 - 15000000) * 0.01 = 4000 JPY
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit, dec!(15400000))
        .await
        .unwrap();
    assert_eq!(closed.pnl_amount, Some(dec!(4000)));

    // Balance: 100000 + 4000 = 104000
    assert_eq!(trader.balance().await, dec!(104000));
}
```

- [ ] **Step 2: PaperTrader を拡張**

`PaperTrader` に `paper_account_id` フィールドを追加:

```rust
pub struct PaperTrader {
    balance: Mutex<Decimal>,
    positions: Mutex<HashMap<Uuid, Trade>>,
    leverage: Decimal,
    paper_account_id: Option<Uuid>,
}

impl PaperTrader {
    pub fn new(initial_balance: Decimal, leverage: Decimal, paper_account_id: Option<Uuid>) -> Self {
        Self {
            balance: Mutex::new(initial_balance),
            positions: Mutex::new(HashMap::new()),
            leverage,
            paper_account_id,
        }
    }

    pub fn account_id(&self) -> Option<Uuid> {
        self.paper_account_id
    }

    pub async fn execute_with_quantity(&self, signal: &Signal, quantity: Decimal) -> anyhow::Result<Trade> {
        // quantity ベースの Trade を作成
        let trade = Trade {
            id: Uuid::new_v4(),
            strategy_name: signal.strategy_name.clone(),
            pair: signal.pair.clone(),
            exchange: Exchange::BitflyerCfd, // caller should set appropriately
            direction: signal.direction,
            entry_price: signal.entry_price,
            exit_price: None,
            stop_loss: signal.stop_loss,
            take_profit: signal.take_profit,
            quantity: Some(quantity),
            leverage: self.leverage,
            fees: Decimal::ZERO,
            paper_account_id: self.paper_account_id,
            entry_at: Utc::now(),
            exit_at: None,
            pnl_pips: None,
            pnl_amount: None,
            exit_reason: None,
            mode: TradeMode::Paper,
            status: TradeStatus::Open,
        };
        self.positions.lock().await.insert(trade.id, trade.clone());
        tracing::info!(
            "Paper OPEN: {} {} {:?} @ {} qty={}",
            trade.strategy_name, trade.pair, trade.direction, trade.entry_price, quantity
        );
        Ok(trade)
    }

    /// Apply overnight fee to all open positions.
    /// Returns total fees charged.
    pub async fn apply_overnight_fees(&self, fee_rate: Decimal) -> Decimal {
        let mut positions = self.positions.lock().await;
        let mut balance = self.balance.lock().await;
        let mut total_fees = Decimal::ZERO;
        for trade in positions.values_mut() {
            let notional = trade.entry_price * trade.quantity.unwrap_or(Decimal::ONE);
            let fee = notional * fee_rate;
            trade.fees += fee;
            *balance -= fee;
            total_fees += fee;
        }
        total_fees
    }
}
```

`close_position` を更新 — quantity がある場合は `pnl_amount = price_diff * quantity`:

```rust
async fn close_position(&self, id: &str, exit_reason: ExitReason, exit_price: Decimal) -> anyhow::Result<Trade> {
    let uuid = Uuid::parse_str(id)?;
    let mut positions = self.positions.lock().await;
    let mut trade = positions
        .remove(&uuid)
        .ok_or_else(|| anyhow::anyhow!("position {id} not found"))?;

    let price_diff = Self::calculate_price_diff(trade.direction, trade.entry_price, exit_price);

    let (pnl_pips, pnl_amount) = if let Some(quantity) = trade.quantity {
        // Crypto/quantity-based: pnl = price_diff * quantity
        (None, price_diff * quantity)
    } else {
        // FX legacy: pip-based calculation
        let pnl_pips = Self::price_diff_to_pips(&trade.pair, price_diff);
        (Some(pnl_pips), price_diff * self.leverage)
    };

    trade.exit_price = Some(exit_price);
    trade.exit_at = Some(Utc::now());
    trade.pnl_pips = pnl_pips;
    // Paper-only scale for FX, actual JPY for crypto
    trade.pnl_amount = Some(pnl_amount);
    trade.exit_reason = Some(exit_reason);
    trade.status = TradeStatus::Closed;

    let mut balance = self.balance.lock().await;
    *balance += pnl_amount;

    tracing::info!(
        "Paper CLOSE: {} {} pnl={} reason={:?}",
        trade.strategy_name, trade.pair, pnl_amount, exit_reason
    );
    Ok(trade)
}
```

- [ ] **Step 3: 既存テストを更新（paper_account_id パラメータ追加）**

全ての `PaperTrader::new(dec!(100000), dec!(25))` を `PaperTrader::new(dec!(100000), dec!(25), None)` に変更。

- [ ] **Step 4: テスト通過を確認**

Run: `cargo test -p auto-trader-executor`
Expected: 全テスト PASS

- [ ] **Step 5: コミット**

```bash
git add -A
git commit -m "feat(executor): extend PaperTrader with quantity, fees, overnight fee, paper_account_id"
```

---

## Task 9: crypto_trend_v1 戦略

**Files:**
- Create: `crates/strategy/src/crypto_trend.rs`
- Modify: `crates/strategy/src/lib.rs`
- Test: `crates/strategy/src/crypto_trend.rs` (モジュール内テスト)

- [ ] **Step 1: テストを書く**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::Candle;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    fn make_crypto_event(close: Decimal, indicators: HashMap<String, Decimal>) -> PriceEvent {
        PriceEvent {
            pair: Pair::new("FX_BTC_JPY"),
            exchange: Exchange::BitflyerCfd,
            candle: Candle {
                pair: Pair::new("FX_BTC_JPY"),
                exchange: Exchange::BitflyerCfd,
                timeframe: "M5".to_string(),
                open: close, high: close, low: close, close,
                volume: Some(100),
                timestamp: Utc::now(),
            },
            indicators,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn no_signal_insufficient_data() {
        let mut strat = CryptoTrendV1::new(
            "test".to_string(), 8, 21, dec!(75), vec![Pair::new("FX_BTC_JPY")],
        );
        let event = make_crypto_event(dec!(15000000), HashMap::new());
        assert!(strat.on_price(&event).await.is_none());
    }

    #[tokio::test]
    async fn ignores_fx_pair() {
        let mut strat = CryptoTrendV1::new(
            "test".to_string(), 8, 21, dec!(75), vec![Pair::new("FX_BTC_JPY")],
        );
        let mut event = make_crypto_event(dec!(150), HashMap::new());
        event.pair = Pair::new("USD_JPY");
        event.exchange = Exchange::Oanda;
        assert!(strat.on_price(&event).await.is_none());
    }
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `cargo test -p auto-trader-strategy crypto_trend`
Expected: FAIL

- [ ] **Step 3: crypto_trend_v1 を実装**

`crates/strategy/src/crypto_trend.rs`:
```rust
use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{MacroUpdate, Strategy};
use auto_trader_core::types::{Direction, Exchange, Pair, Signal};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};

pub struct CryptoTrendV1 {
    name: String,
    ma_short_period: usize,
    ma_long_period: usize,
    rsi_threshold: Decimal,
    pairs: Vec<Pair>,
    price_history: HashMap<String, VecDeque<Decimal>>,
}

impl CryptoTrendV1 {
    pub fn new(
        name: String,
        ma_short: usize,
        ma_long: usize,
        rsi_threshold: Decimal,
        pairs: Vec<Pair>,
    ) -> Self {
        Self {
            name,
            ma_short_period: ma_short,
            ma_long_period: ma_long,
            rsi_threshold,
            pairs,
            price_history: HashMap::new(),
        }
    }
}

#[async_trait::async_trait]
impl Strategy for CryptoTrendV1 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_price(&mut self, event: &PriceEvent) -> Option<Signal> {
        if event.exchange != Exchange::BitflyerCfd {
            return None;
        }
        if !self.pairs.iter().any(|p| p == &event.pair) {
            return None;
        }

        let key = event.pair.0.clone();
        let history = self.price_history.entry(key).or_default();
        history.push_back(event.candle.close);

        let max_len = self.ma_long_period + 2;
        while history.len() > max_len {
            history.pop_front();
        }

        let closes: Vec<Decimal> = history.iter().copied().collect();
        if closes.len() < self.ma_long_period + 1 {
            return None;
        }

        let sma_short = auto_trader_market::indicators::sma(&closes, self.ma_short_period)?;
        let sma_long = auto_trader_market::indicators::sma(&closes, self.ma_long_period)?;
        let rsi = event.indicators.get("rsi_14")?;

        let prev_closes = &closes[..closes.len() - 1];
        let prev_sma_short = auto_trader_market::indicators::sma(prev_closes, self.ma_short_period)?;
        let prev_sma_long = auto_trader_market::indicators::sma(prev_closes, self.ma_long_period)?;

        let golden_cross = prev_sma_short <= prev_sma_long && sma_short > sma_long;
        let death_cross = prev_sma_short >= prev_sma_long && sma_short < sma_long;

        let entry = event.candle.close;
        // Crypto uses price_unit=1 (JPY), so SL/TP in absolute JPY terms
        let sl_distance = entry * Decimal::new(2, 2); // 2% of price
        let tp_distance = entry * Decimal::new(4, 2); // 4% of price (2:1 R:R)

        if golden_cross && rsi < &self.rsi_threshold {
            Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Long,
                entry_price: entry,
                stop_loss: entry - sl_distance,
                take_profit: entry + tp_distance,
                confidence: 0.7,
                timestamp: event.timestamp,
            })
        } else if death_cross && rsi > &(Decimal::from(100) - self.rsi_threshold) {
            Some(Signal {
                strategy_name: self.name.clone(),
                pair: event.pair.clone(),
                direction: Direction::Short,
                entry_price: entry,
                stop_loss: entry + sl_distance,
                take_profit: entry - tp_distance,
                confidence: 0.7,
                timestamp: event.timestamp,
            })
        } else {
            None
        }
    }

    fn on_macro_update(&mut self, _update: &MacroUpdate) {
        // Crypto strategy ignores macro updates (per spec: algorithm evolution only)
    }
}
```

- [ ] **Step 4: lib.rs にモジュール追加**

```rust
pub mod crypto_trend;
```

- [ ] **Step 5: テスト通過を確認**

Run: `cargo test -p auto-trader-strategy crypto_trend`
Expected: PASS

- [ ] **Step 6: コミット**

```bash
git add -A
git commit -m "feat(strategy): add CryptoTrendV1 (MA cross + RSI for BTC/JPY)"
```

---

## Task 10: main.rs パイプライン配線

**Files:**
- Modify: `crates/app/src/main.rs`
- Modify: `crates/app/Cargo.toml`

- [ ] **Step 1: main.rs に bitFlyer 監視タスクを追加**

`main()` 内、MarketMonitor spawn の後に:

```rust
// bitFlyer monitor (crypto)
let bitflyer_handle = if let Some(bf_config) = &config.bitflyer {
    let crypto_pairs: Vec<Pair> = config.pairs.crypto.as_ref()
        .map(|v| v.iter().map(|s| Pair::new(s)).collect())
        .unwrap_or_default();
    if !crypto_pairs.is_empty() {
        let bf_monitor = auto_trader_market::bitflyer::BitflyerMonitor::new(
            &bf_config.ws_url,
            crypto_pairs,
            "M5",
            price_tx.clone(),
        );
        let bf_monitor = if let Some(pool) = Some(pool.clone()) {
            bf_monitor.with_db(pool)
        } else {
            bf_monitor
        };
        Some(tokio::spawn(async move {
            if let Err(e) = bf_monitor.run().await {
                tracing::error!("bitflyer monitor error: {e}");
            }
        }))
    } else {
        None
    }
} else {
    None
};
```

- [ ] **Step 2: 複数 PaperAccount のワイヤリング**

設定ファイルの `[[paper_accounts]]` から PaperTrader インスタンスを生成し、同じシグナルを全口座に配信:

```rust
// Paper accounts (crypto)
let paper_accounts: Vec<(String, Arc<PaperTrader>)> = {
    let mut accounts = Vec::new();
    for pac in &config.paper_accounts {
        let id = Uuid::new_v4(); // or load from DB
        let trader = Arc::new(PaperTrader::new(
            pac.initial_balance,
            pac.leverage,
            Some(id),
        ));
        accounts.push((pac.name.clone(), trader));
        tracing::info!("paper account: {} (balance={}, leverage={})", pac.name, pac.initial_balance, pac.leverage);
    }
    accounts
};
```

Signal executor タスクを拡張して、crypto シグナルを全 paper_accounts に配信:

```rust
// シグナルが crypto ペアの場合、全 paper_accounts に配信
// シグナルが FX ペアの場合、既存の paper_trader に配信
```

- [ ] **Step 3: crypto_trend_v1 のストラテジー登録を追加**

既存の strategy engine 登録ロジックに `crypto_trend` を追加:

```rust
name if name.starts_with("crypto_trend") => {
    let ma_short = sc.params.get("ma_short")
        .and_then(|v| v.as_integer()).unwrap_or(8) as usize;
    let ma_long = sc.params.get("ma_long")
        .and_then(|v| v.as_integer()).unwrap_or(21) as usize;
    let rsi_thresh = sc.params.get("rsi_threshold")
        .and_then(|v| v.as_integer()).unwrap_or(75);
    let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
    engine.add_strategy(
        Box::new(auto_trader_strategy::crypto_trend::CryptoTrendV1::new(
            sc.name.clone(), ma_short, ma_long, Decimal::from(rsi_thresh), pairs,
        )),
        sc.mode.clone(),
    );
    tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
}
```

- [ ] **Step 4: オーバーナイト手数料タスクを追加**

Daily batch タスクに併設:

```rust
// Task: Overnight fee (crypto paper accounts)
// Apply 0.04%/day fee to open positions at UTC 0:00
let overnight_accounts = paper_accounts.clone();
let overnight_handle = tokio::spawn(async move {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
    let mut last_date = chrono::Utc::now().date_naive();
    let fee_rate = Decimal::new(4, 4); // 0.04%
    loop {
        interval.tick().await;
        let today = chrono::Utc::now().date_naive();
        if today != last_date {
            for (name, trader) in &overnight_accounts {
                let fees = trader.apply_overnight_fees(fee_rate).await;
                if fees > Decimal::ZERO {
                    tracing::info!("overnight fee applied: {} = {} JPY", name, fees);
                }
            }
            last_date = today;
        }
    }
});
```

- [ ] **Step 5: shutdown に bitflyer_handle と overnight_handle を追加**

```rust
if let Some(h) = bitflyer_handle {
    h.abort();
}
overnight_handle.abort();
```

- [ ] **Step 6: cargo check を確認**

Run: `cargo check`
Expected: 成功

- [ ] **Step 7: cargo test で全テスト通過を確認**

Run: `cargo test`
Expected: 全テスト PASS

- [ ] **Step 8: コミット**

```bash
git add -A
git commit -m "feat(app): wire bitFlyer monitor, crypto paper accounts, overnight fees"
```

---

## Task 11: Vegapunk 連携の拡張

**Files:**
- Modify: `crates/app/src/main.rs` (executor/recorder タスク内)

- [ ] **Step 1: Vegapunk ingest に exchange メタデータを追加**

executor タスク内の Vegapunk ingest テキストに exchange 情報を含める:

```rust
let text = format!(
    "[{}] {} {} 判断。trade_id: {}。エントリー価格: {}。qty: {}。SL: {}、TP: {}。戦略: {}",
    trade.exchange, trade.pair, direction_str,
    trade.id, trade.entry_price,
    trade.quantity.map(|q| q.to_string()).unwrap_or_default(),
    trade.stop_loss, trade.take_profit, trade.strategy_name
);
let channel = format!("{}-trades", trade.pair.0.to_lowercase());
```

recorder タスクも同様に exchange を含める。

- [ ] **Step 2: cargo check を確認**

Run: `cargo check`
Expected: 成功

- [ ] **Step 3: コミット**

```bash
git add -A
git commit -m "feat(vegapunk): add exchange metadata to trade ingestion"
```

---

## Task 12: 統合テスト

**Files:**
- Modify: `crates/app/tests/integration_test.rs`

- [ ] **Step 1: crypto paper trade のテストを追加**

```rust
#[tokio::test]
async fn crypto_paper_trade_with_quantity() {
    let trader = PaperTrader::new(dec!(100000), dec!(2), Some(Uuid::new_v4()));
    let signal = Signal {
        strategy_name: "crypto_trend_v1".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        entry_price: dec!(15000000),
        stop_loss: dec!(14800000),
        take_profit: dec!(15400000),
        confidence: 0.8,
        timestamp: Utc::now(),
    };
    let trade = trader.execute_with_quantity(&signal, dec!(0.01)).await.unwrap();
    assert_eq!(trade.status, TradeStatus::Open);
    assert_eq!(trade.exchange, Exchange::BitflyerCfd);
    assert_eq!(trade.quantity, Some(dec!(0.01)));

    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit, dec!(15400000))
        .await
        .unwrap();
    assert_eq!(closed.pnl_amount, Some(dec!(4000)));
    assert_eq!(trader.balance().await, dec!(104000));
}

#[tokio::test]
async fn overnight_fee_deducted() {
    let trader = PaperTrader::new(dec!(100000), dec!(2), Some(Uuid::new_v4()));
    let signal = Signal {
        strategy_name: "test".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        entry_price: dec!(15000000),
        stop_loss: dec!(14800000),
        take_profit: dec!(15400000),
        confidence: 0.8,
        timestamp: Utc::now(),
    };
    trader.execute_with_quantity(&signal, dec!(0.01)).await.unwrap();

    // Overnight fee: 15000000 * 0.01 * 0.0004 = 60 JPY
    let fees = trader.apply_overnight_fees(dec!(0.0004)).await;
    assert_eq!(fees, dec!(60));
    assert_eq!(trader.balance().await, dec!(99940));
}
```

- [ ] **Step 2: テスト通過を確認**

Run: `cargo test -p auto-trader`
Expected: 全テスト PASS

- [ ] **Step 3: コミット**

```bash
git add -A
git commit -m "test: add crypto paper trade and overnight fee integration tests"
```

---

## Task 13: specs を配置し最終確認

**Files:**
- Already placed: `specs/crypto-paper-trading.md`

- [ ] **Step 1: specs をコミット**

```bash
git add specs/crypto-paper-trading.md
git commit -m "docs: add crypto paper trading spec"
```

- [ ] **Step 2: 全テスト通過を最終確認**

Run: `cargo test`
Expected: 全テスト PASS（既存24 + 新規テスト）

- [ ] **Step 3: cargo clippy で警告がないことを確認**

Run: `cargo clippy -- -D warnings`
Expected: 警告なし（または既存と同等）
