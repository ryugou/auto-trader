# シミュレーション基盤 Implementation Plan

> **For agentic workers:** この計画は vibepod コンテナ内で、kaneko（実装、sonnet）→ reviewer（レビュー、opus）の順に、タスク単位で実行する。各タスクは TDD（失敗するテストを先に書く）で進める。Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 過去の USD/JPY 5 分足に Rhai スクリプトのアルゴリズムを流し、折り返しから計算した実質上限と比べて評価する `auto-trader-sim` を作る。

**Architecture:** 新しい crate `crates/sim` に、データ取得、足の系列と指標、折り返しの基準値、スクリプトの隔離実行、シミュレーション、評価、パラメータ探索、保存、CLI を、モジュールごとに分けて実装する。価格は整数（ミリ円）で扱い、指標とスクリプトだけが `f64` を使う。既存の crate は、指標の一致テストで `auto-trader-market` を参照する以外に触れない。

**Tech Stack:** Rust 2024（rust-version 1.85）、sqlx 0.8（PostgreSQL）、reqwest 0.12、rhai 1（`sync`）、rayon 1、rand 0.9、rand_chacha 0.9、clap 4（`derive`）、wiremock 0.6（テスト）

**Spec:** [`docs/superpowers/specs/2026-10-01-simulation-foundation-design.md`](../specs/2026-10-01-simulation-foundation-design.md)。この計画の「spec N 章」はこの文書の章を指す。定義・数式・テーブル・コマンドの正本は spec であり、この計画は複製しない。

## Global Constraints

- 実装者は、作業前に spec の全文と、この計画の担当タスクを読む。spec と計画が食い違う場合は spec を正とし、最終出力で食い違いを報告する。
- spec にない非自明な判断が必要になったら、実装せずに差し戻す。
- 既存の crate、既存のテーブル、`crates/backtest`、`price_candles` を変更しない。変更してよい既存ファイルは、ルートの `Cargo.toml`、`Dockerfile`、`config/default.toml` だけである。
- 価格の比較・加減算は `i64`（ミリ円、または `mid2`）で行う。`f64` は指標、スクリプトへの受け渡し、pips への最終変換だけに使う（spec 3 章）。
- エラーをログなしで握りつぶさない。エラーを既定値に置き換えて続行しない（spec 8.4）。
- パス、URL、上限値をコード内にハードコードしない。設定値は `SimConfig`、定数は `types.rs` に置く。
- 各タスクの完了条件は `./scripts/test-all.sh` が `ALL GREEN` で終了することである。個別の `cargo` コマンドは開発中の確認にだけ使う。
- コミットは Conventional Commits とし、本文に `Refs #98` を含める。push と PR 作成は行わない。
- Python を使わない。`cd` と `git -C` を使わない。

## Review Focus

spec が含意するが、どのタスクのテストも踏まない恐れが高い入力を、起きやすい順に挙げる。各行のテストは、括弧内のタスクに含めてある。

1. 週末などで足が飛ぶ区間をまたぐ約定と保護ストップ。次に存在する足の始値で約定し、始値がストップを飛び越えていれば始値で決済する（Task 6）。
2. 読み込み範囲の先頭が上位足のバケットの途中から始まる場合。先頭の上位足は存在する M5 だけで作られ、完成判定は終了時刻だけで決まる（Task 3）。
3. スクリプトが指標の `()`（本数不足）をそのまま演算に使った場合。既定値に置き換えず `script_error` になる（Task 5）。
4. 評価期間内に折り返しが 1 つも確定しない場合。波が 0 件で、`capture_rate` などが null になり、panic しない（Task 4、Task 7）。
5. `sweep` の全組み合わせ数が `max_runs` より 1 だけ大きい場合と、パラメータが 0 個の場合。前者は `default` を含む `max_runs` 件、後者は 1 件を実行する（Task 8）。

## File Structure

| ファイル | 責務 |
| --- | --- |
| `Cargo.toml`（ルート） | `members` に `crates/sim` を追加。`[workspace.dependencies]` に `rhai`、`rayon`、`rand`、`rand_chacha`、`clap`、`auto-trader-sim` を追加 |
| `Dockerfile` | ビルド行と COPY 行に `auto-trader-sim` を追加 |
| `config/default.toml` | `[sim]` 節を追加（spec 4 章の内容） |
| `migrations/20261001000001_simulation_foundation.sql` | spec 5.1 と 12 章の 4 テーブル |
| `crates/sim/Cargo.toml` | パッケージ定義。`[lib]` と `[[bin]] name = "auto-trader-sim"` |
| `crates/sim/src/lib.rs` | モジュール宣言 |
| `crates/sim/src/error.rs` | `SimError` |
| `crates/sim/src/types.rs` | `Bar`、単位の定数、pips 変換 |
| `crates/sim/src/config.rs` | `SimConfig`、`Settings`、読み込みと検証 |
| `crates/sim/src/data.rs` | DB 接続、テーブル確認、`sim_candles` の読み書き |
| `crates/sim/src/fetch.rs` | GMO KLine の取得、結合、検証、`backfill` |
| `crates/sim/src/series.rs` | `Tf`、`TfSeries`、`Dataset`、上位足の集約と完成判定 |
| `crates/sim/src/indicators.rs` | 8 種の指標系列（`f64`）と `IndicatorCache` |
| `crates/sim/src/benchmark.rs` | 折り返しの抽出、波、理論値、実質上限、方向ラベル |
| `crates/sim/src/script.rs` | Rhai エンジンの構成、スクリプトの検証、`ctx`、実行 |
| `crates/sim/src/engine.rs` | 1 回のシミュレーション |
| `crates/sim/src/eval.rs` | 評価指標 |
| `crates/sim/src/sweep.rs` | 組み合わせの列挙と抽出、並列実行 |
| `crates/sim/src/store.rs` | `sim_scripts`、`sim_batches`、`sim_runs` の読み書き |
| `crates/sim/src/cli.rs` | サブコマンドの定義と実行、出力、終了コード |
| `crates/sim/src/main.rs` | `cli::run` を呼び、終了コードを返す |
| `crates/sim/scripts/donchian_sar.rhai` | spec 8.1 のスクリプト |
| `crates/sim/tests/*.rs` | DB とモックサーバーを使う結合テスト |

## 共通の型（Task 1 で定義し、以降のタスクが使う）

```rust
// error.rs
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("config: {0}")]
    Config(String),
    #[error("argument: {0}")]
    Args(String),
    #[error("missing tables: {0:?}. run the new auto-trader image once (it applies migrations), or `auto-trader-sim migrate` on a non-production database")]
    MissingTables(Vec<String>),
    #[error("invalid_script: {0}")]
    InvalidScript(String),
    #[error("fetch incomplete: {0:?}")]
    FetchIncomplete(Vec<String>),
    #[error("batch failed: {0}")]
    BatchFailed(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// types.rs
pub const PIP_MILLI: i64 = 10; // 1 pip = 10 ミリ円
pub const PIP_MID2: i64 = 20;  // 1 pip = mid2 の 20 単位
pub const M5_SECS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub open_time: i64, // UTC エポック秒
    pub bid_open: i64,  // 以下すべてミリ円
    pub bid_high: i64,
    pub bid_low: i64,
    pub bid_close: i64,
    pub ask_open: i64,
    pub ask_high: i64,
    pub ask_low: i64,
    pub ask_close: i64,
}
impl Bar {
    pub fn mid2_open(&self) -> i64;
    pub fn mid2_high(&self) -> i64;
    pub fn mid2_low(&self) -> i64;
    pub fn mid2_close(&self) -> i64;
}
pub fn milli_to_pips(v: i64) -> f64; // v / 10.0
pub fn mid2_to_pips(v: i64) -> f64;  // v / 20.0
pub fn mid2_to_yen(v: i64) -> f64;   // v / 2000.0
```

---

### Task 1: crate の骨組み、設定、マイグレーション

**Files:**
- Create: `crates/sim/Cargo.toml`、`crates/sim/src/{lib,main,error,types,config}.rs`、`migrations/20261001000001_simulation_foundation.sql`
- Modify: ルート `Cargo.toml`、`Dockerfile`、`config/default.toml`

**Interfaces:**
- Produces: 上記「共通の型」と、次の設定 API。

```rust
// config.rs
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct SimConfig {
    pub gmo_public_base_url: String,
    pub warmup_bars: usize,
    pub protective_stop_pips: i64,
    pub thetas_pips: Vec<i64>,
    pub jobs: usize,
    pub max_operations_per_bar: u64,
    pub max_operations_per_run: u64,
    pub indicator_cache_mb: usize,
}
impl Default for SimConfig; // spec 4 章の値
impl SimConfig {
    pub fn validate(&self) -> Result<(), SimError>; // spec 4 章の条件。違反は SimError::Config
}
pub struct Settings {
    pub database_url: String,
    pub sim: SimConfig,
}
pub fn load(path: &std::path::Path) -> Result<Settings, SimError>; // [database].url と [sim] だけを読み、validate まで行う
pub fn config_path() -> std::path::PathBuf; // CONFIG_PATH、未設定なら config/default.toml
```

- [ ] **Step 1:** ルート `Cargo.toml` に member と依存を追加し、`crates/sim/Cargo.toml` を作る。依存は `sqlx`、`reqwest`、`tokio`、`serde`、`serde_json`、`toml`、`chrono`、`uuid`、`rust_decimal`、`tracing`、`tracing-subscriber`、`anyhow`、`thiserror`、`sha2`、`hex`（すべて `workspace = true`）と、新規の `rhai`、`rayon`、`rand`、`rand_chacha`、`clap`。dev-dependencies は `wiremock`、`auto-trader-market`。
- [ ] **Step 2:** `config.rs` の失敗するテストを書く。
  - `[sim]` 節がない TOML で、`SimConfig::default()` と等しい値が読める。
  - `[sim]` に一部のキーだけがある TOML で、残りが既定値になる。
  - `[database].url` がない TOML が `SimError::Config` になる。
  - 検証違反が 1 件ずつ `SimError::Config` になる: `warmup_bars = 0`、`protective_stop_pips = 0`、`thetas_pips = []`、`thetas_pips = [20, 20]`、`thetas_pips = [0]`、`jobs = 0`、`max_operations_per_bar = 0`、`max_operations_per_run = 0`、`indicator_cache_mb = 0`。
  - リポジトリの `config/default.toml` が読め、`[sim]` の値が `SimConfig::default()` と一致する。
- [ ] **Step 3:** `types.rs` の失敗するテストを書く。`Bar { bid_close: 150_000, ask_close: 150_004, .. }` の `mid2_close()` が `300_004`、`mid2_to_yen(300_004)` が `150.002`、`milli_to_pips(25)` が `2.5`、`mid2_to_pips(50)` が `2.5`。
- [ ] **Step 4:** `error.rs`、`types.rs`、`config.rs` を実装し、テストを通す。
- [ ] **Step 5:** `migrations/20261001000001_simulation_foundation.sql` を spec 5.1 と 12 章の SQL のとおりに作る。
- [ ] **Step 6:** `config/default.toml` に spec 4 章の `[sim]` 節を追加する。`Dockerfile` のビルド行を `cargo build --release --bin auto-trader --bin auto-trader-sim` にし、`auto-trader-sim` を `/usr/local/bin/` へ COPY する行を追加する。
- [ ] **Step 7:** `main.rs` は、この時点では `fn main() {}` 相当の最小実装とする（Task 9 で置き換える）。
- [ ] **Step 8:** `./scripts/test-all.sh` を実行し、`ALL GREEN` を確認してコミットする（`feat(sim): scaffold simulation crate, config and migration`）。

### Task 2: データの保存と取得

**Files:**
- Create: `crates/sim/src/data.rs`、`crates/sim/src/fetch.rs`、`crates/sim/tests/data_test.rs`、`crates/sim/tests/fetch_test.rs`

**Interfaces:**
- Consumes: `Bar`、`SimError`、`SimConfig`
- Produces:

```rust
// data.rs
pub async fn connect(database_url: &str) -> Result<sqlx::PgPool, SimError>; // マイグレーションを実行しない。max_connections = 5
pub async fn ensure_tables(pool: &sqlx::PgPool) -> Result<(), SimError>;    // 4 テーブルのうち欠けているものを MissingTables で返す
pub async fn upsert_bars(pool: &sqlx::PgPool, bars: &[Bar]) -> Result<u64, SimError>;
pub async fn load_bars(pool: &sqlx::PgPool, to_exclusive: Option<i64>) -> Result<Vec<Bar>, SimError>; // open_time 昇順

// fetch.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceType { Bid, Ask }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kline { pub open_time: i64, pub open: i64, pub high: i64, pub low: i64, pub close: i64 } // 秒、ミリ円
pub struct GmoKlineClient { /* base_url, http, min_interval, retry_delays */ }
impl GmoKlineClient {
    pub fn new(base_url: &str) -> Self; // min_interval = 1 秒、retry_delays = [2s, 4s, 8s]
    pub fn with_timing(self, min_interval: std::time::Duration, retry_delays: Vec<std::time::Duration>) -> Self;
    pub async fn fetch_day(&self, date: chrono::NaiveDate, price_type: PriceType) -> Result<Vec<Kline>, String>;
}
pub struct JoinOutcome { pub bars: Vec<Bar>, pub one_sided: usize, pub invalid: usize }
pub fn join_and_validate(bid: &[Kline], ask: &[Kline]) -> JoinOutcome;
pub struct BackfillReport { pub days: usize, pub saved: u64, pub one_sided: usize, pub invalid: usize, pub failed: Vec<String> } // failed は "YYYYMMDD BID" の形
pub async fn backfill(pool: &sqlx::PgPool, client: &GmoKlineClient, from: chrono::NaiveDate, to: chrono::NaiveDate) -> Result<BackfillReport, SimError>;
```

`fetch_day` が読むレスポンスの形（テストのフィクスチャにも使う）:

```json
{"status":0,"data":[{"openTime":"1698451200000","open":"149.605","high":"149.612","low":"149.601","close":"149.610"}],"responsetime":"2023-10-28T00:05:00.000Z"}
```

`openTime` はミリ秒の文字列で、秒に変換する。価格の文字列は `rust_decimal` で読み、1000 倍が整数にならない値は不正な足として扱う。

- [ ] **Step 1:** `join_and_validate` の失敗する単体テストを書く。
  - 同じ `open_time` の BID と ASK が 1 本の `Bar` になる。
  - BID だけにある足と ASK だけにある足が、それぞれ `one_sided` に数えられ、`bars` に入らない。
  - spec 5.2 の不正条件が 1 件ずつ `invalid` に数えられる: 価格 0、`high < max(open, close)`、`low > min(open, close)`、open・high・low・close のいずれかで売値が買値より小さい。
  - 結果が `open_time` の昇順である。
- [ ] **Step 2:** `fetch_test.rs` の失敗するテストを書く（wiremock）。`with_timing` で間隔と再試行を 1 ミリ秒にする。
  - 正常なレスポンスが `Kline` に変換される（上のフィクスチャで `open_time = 1698451200`、`open = 149_605`）。
  - HTTP 500 が 2 回続いた後に成功すると、結果が返る（リクエストは 3 回）。
  - `status` が 5 のレスポンスが 4 回続くと `Err` になる（リクエストは 4 回）。
  - `data` が空配列のレスポンスが、空の `Vec` として成功する。
- [ ] **Step 3:** `data_test.rs` の失敗するテストを書く（`#[sqlx::test(migrations = "../../migrations")]`）。
  - `upsert_bars` で保存した足が `load_bars` で同じ値・昇順で読める。
  - 同じ足をもう一度 `upsert_bars` しても行数が増えず、値を変えて保存すると更新される。
  - `load_bars(Some(t))` が `open_time < t` の足だけを返す。
  - `ensure_tables` が、マイグレーション済みの DB で `Ok` を返す。
  - `sim_runs` を DROP した DB で、`ensure_tables` が `MissingTables(["sim_runs"])` を返す。
- [ ] **Step 4:** `backfill` の失敗するテストを書く（wiremock と `#[sqlx::test]`）。
  - 2 日分の BID・ASK が保存され、`BackfillReport` の件数が一致する。
  - 1 日の ASK だけが失敗し続ける場合に、`failed == ["<その日付> ASK"]` となり、他の日は保存され、戻り値が `Err(SimError::FetchIncomplete(..))` になる。
- [ ] **Step 5:** `data.rs` と `fetch.rs` を実装し、テストを通す。WARN と ERROR のログには、日付、`priceType`、件数、HTTP ステータスまたは `status` の値を含める。
- [ ] **Step 6:** `./scripts/test-all.sh` が `ALL GREEN` であることを確認してコミットする（`feat(sim): add candle storage and GMO kline backfill`）。

### Task 3: 足の系列、上位足、指標

**Files:**
- Create: `crates/sim/src/series.rs`、`crates/sim/src/indicators.rs`

**Interfaces:**
- Consumes: `Bar`、`SimError`、単位の定数
- Produces:

```rust
// series.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tf { M5, M15, H1, H4 }
impl Tf {
    pub fn secs(self) -> i64;                 // 300, 900, 3600, 14400
    pub fn parse(s: &str) -> Option<Tf>;      // "M5" | "M15" | "H1" | "H4"
}
pub struct TfSeries {
    pub open: Vec<f64>, pub high: Vec<f64>, pub low: Vec<f64>, pub close: Vec<f64>, // 円
    pub end_time: Vec<i64>,                   // バケットの終了時刻（UTC エポック秒）
}
pub struct Dataset { /* bars, eval_start, series, completed, cache */ }
impl Dataset {
    /// all_bars は open_time 昇順。評価期間 [from, to) と、その前の warmup_bars 本だけを保持する。
    /// from より前の足が warmup_bars 本に満たなければ SimError::Args（指定可能な最も早い from の日付を含める）。
    /// 評価期間内の足が 0 本でも SimError::Args。
    pub fn new(all_bars: Vec<Bar>, from: i64, to: i64, warmup_bars: usize, cache_mb: usize) -> Result<Dataset, SimError>;
    pub fn bars(&self) -> &[Bar];             // warmup を含む読み込み範囲
    pub fn eval_start(&self) -> usize;        // 評価期間の先頭の添字（= warmup_bars）
    pub fn eval_bars(&self) -> &[Bar];        // 評価期間だけ
    pub fn series(&self, tf: Tf) -> &TfSeries;
    pub fn completed(&self, tf: Tf, t: usize) -> usize; // 足 t の時点で完成している上位足の本数
    pub fn indicator(&self, key: IndicatorKey) -> std::sync::Arc<IndicatorSeries>;
    pub fn missing_weekdays(&self) -> Vec<chrono::NaiveDate>; // 評価期間内で足が 0 本の平日（UTC）
}

// indicators.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndicatorKind { Sma, Ema, Rsi, Atr, Adx, Bb, Donchian, Keltner }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndicatorKey { pub tf: Tf, pub kind: IndicatorKind, pub period: u32, pub mult_x100: u32 } // mult を使わない指標は 0
pub enum IndicatorSeries {
    Single(Vec<f64>),
    Band { lower: Vec<f64>, middle: Vec<f64>, upper: Vec<f64> }, // bb, keltner
    Channel { lower: Vec<f64>, upper: Vec<f64> },                // donchian
}
// 値が存在しない要素（本数不足）は f64::NAN とする。
pub fn compute(series: &TfSeries, key: IndicatorKey) -> IndicatorSeries;
```

実装上の要点:

- 指標は、系列全体を 1 回の走査で計算する。要素 `i` の値は、`crates/market/src/indicators.rs` の対応する関数を系列の `[0..=i]` に適用した値と一致しなければならない（spec 6.3 の対応表）。`indicators.rs` の各関数の計算方法（初期値の作り方、平滑化）を読み、同じ結果になる逐次計算を実装する。
- `IndicatorCache` は、キーごとに 1 度だけ計算し、複数スレッドから共有できる。合計サイズが `cache_mb` を超えたら、最後に使われてから最も時間がたった系列から破棄する。

- [ ] **Step 1:** 集約の失敗するテストを書く。
  - M5 が 3 本そろった M15 の open・high・low・close が、最初の open、最大の high、最小の low、最後の close（いずれも中値）になる。
  - 読み込み範囲の先頭がバケットの途中（例: 分が 05 の足）から始まる場合、先頭の M15 は存在する 2 本だけで作られる。
  - 足が 1 本もないバケットの上位足が作られない（週末を挟むデータ）。
  - 完成判定: 分が 00・05・10 の 3 本について、`completed(M15, t)` が 00 と 05 の足では直前までの本数、10 の足で 1 増える。
  - 未来非依存: 足 `t` より後の足の値を書き換えた `Dataset` でも、`completed(tf, t)` と、その範囲の上位足の値が変わらない（4 つの時間足すべて）。
- [ ] **Step 2:** `Dataset::new` の失敗するテストを書く。
  - `from` より前の足が `warmup_bars` 本ちょうどある場合に成功し、`eval_start() == warmup_bars`、`bars().len() == warmup_bars + 評価期間の本数` になる。
  - 1 本足りない場合に `SimError::Args` になり、メッセージに指定可能な最も早い日付が含まれる。
  - 評価期間内の足が 0 本の場合に `SimError::Args` になる。
  - `missing_weekdays` が、足のない平日だけを返し、土日を返さない。
- [ ] **Step 3:** 指標の失敗するテストを書く。乱数ではなく決定的な式（例: `150.0 + 0.3 * sin(i * 0.07) + 0.001 * i`）で 400 本の系列を作り、次を確認する。
  - 8 種それぞれについて、複数の `period`（2、14、50）と、添字 `i`（`period - 1`、`period`、`2 * period`、`2 * period + 1`、399）で、`auto_trader_market::indicators` の対応する関数を `[0..=i]` に適用した値との差が `1e-6` 以下である。既存関数が `None` を返す添字では、こちらが NaN である。
  - `Band` と `Channel` の `lower`・`middle`・`upper` が、既存関数のタプルの同名の要素に対応する（spec 6.3 の表）。
  - `donchian` が `include_current = true` の結果と一致する。
- [ ] **Step 4:** キャッシュの失敗するテストを書く。
  - 同じキーを 2 回要求すると、同じ `Arc` が返る。
  - 8 スレッドから同じキーを同時に要求しても、計算が 1 回だけ行われる（計算回数のカウンタで確認）。
  - 容量を 1 MB にして多数のキーを要求すると、古いキーが破棄され、破棄後に再要求した値が最初の値と一致する。
- [ ] **Step 5:** `series.rs` と `indicators.rs` を実装し、テストを通す。
- [ ] **Step 6:** `./scripts/test-all.sh` が `ALL GREEN` であることを確認してコミットする（`feat(sim): add bar series, timeframe aggregation and indicators`）。

### Task 4: 折り返しと基準値

**Files:**
- Create: `crates/sim/src/benchmark.rs`

**Interfaces:**
- Consumes: `Bar`、`PIP_MID2`
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leg {
    pub a: usize,               // 始点（評価期間内の添字）
    pub b: usize,               // 終点
    pub direction: i8,          // 上昇 = 1、下降 = -1
    pub ideal_milli: i64,
    pub realizable_milli: i64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Benchmark {
    pub theta_pips: i64,
    pub legs: Vec<Leg>,         // spec 7.1 で波として扱うものだけ
    pub ideal_milli: i64,
    pub realizable_milli: i64,
    pub labels: Vec<i8>,        // 評価期間の各足。ラベルなしは 0
}
pub fn compute(eval_bars: &[Bar], theta_pips: i64) -> Benchmark;
```

- [ ] **Step 1:** 失敗するテストを書く。テスト用の `Bar` は、中値の終値（円）とスプレッド（ミリ円）から作るヘルパーで生成する。始値は直前の足の終値と同じにする。
  - 中値の終値が `150.00, 150.10, 150.30, 150.20, 150.05, 150.15, 150.40, 150.10` で、`theta_pips = 20`、スプレッド 0 の場合:
    - 最初の折り返し点は添字 0（安値）。150.30 − 150.00 が 30 pips で、添字 2 の時点で上昇に確定する。
    - 添字 2 が高値として確定するのは、添字 4（150.05、25 pips の逆行）の時点である。
    - 添字 4 が安値として確定するのは、添字 6（150.40、35 pips の上昇）の時点である。
    - 添字 6 が高値として確定するのは、添字 7（150.10、30 pips の逆行）の時点である。
    - 確定した折り返し点は 0、2、4、6。波の候補は 0→2（上昇）、2→4（下降）、4→6（上昇）。4→6 は足 `b+1 = 7` が存在するので波として扱う。
    - `legs.len() == 3`。理論値は 30 + 25 + 35 = 90 pips（`ideal_milli == 900`）。
    - 方向ラベルは、添字 1・2 が 1、添字 3・4 が −1、添字 5・6 が 1、添字 0 と 7 が 0。
  - 同じ系列でスプレッドを 4 ミリ円（0.4 pips）にすると、理論値が波 1 つにつき 4 ミリ円ずつ減る。
  - 実質上限が、各波の足 `a+1` と足 `b+1` の始値（買値・売値）から、spec 7.2 の式どおりに計算される（上の系列で期待値を手計算してテストに書く）。
  - 逆行がちょうど 20 pips（`mid2` の差が 400）の場合に折り返しとなり、19.95 pips（差が 399）ではならない。
  - 同値の極値が 2 回現れる場合に、先の位置が折り返し点になる。
  - 最後の波の足 `b+1` が存在しない場合、その波が `legs` に入らず、ラベルも付かない。
  - 値動きが `theta` に満たない系列で、`legs` が空、合計が 0、ラベルがすべて 0 になる。
  - 空の入力と 1 本だけの入力で panic しない。
- [ ] **Step 2:** `benchmark.rs` を実装し、テストを通す。判定はすべて `i64` で行う。
- [ ] **Step 3:** `./scripts/test-all.sh` が `ALL GREEN` であることを確認してコミットする（`feat(sim): add swing extraction and benchmark values`）。

### Task 5: スクリプトの隔離実行

**Files:**
- Create: `crates/sim/src/script.rs`、`crates/sim/scripts/donchian_sar.rhai`

**Interfaces:**
- Consumes: `Dataset`、`Tf`、`IndicatorKey`、`IndicatorKind`、`IndicatorSeries`、`SimConfig`、`SimError`
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind { Int, Float }
#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec { pub name: String, pub kind: ParamKind, pub min: f64, pub max: f64, pub step: f64, pub default: f64 }
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue { Int(i64), Float(f64) }
pub type ParamSet = std::collections::BTreeMap<String, ParamValue>;

pub struct CompiledScript {
    pub source: String,
    pub sha256: String,           // 小文字の 16 進
    pub params: Vec<ParamSpec>,   // 名前の辞書順
    /* ast */
}
impl CompiledScript {
    pub fn default_params(&self) -> ParamSet;
    /// spec 13 章の --params の規則で検証し、指定のないものを default で埋める。違反は SimError::Args。
    pub fn resolve_params(&self, given: &serde_json::Map<String, serde_json::Value>) -> Result<ParamSet, SimError>;
}

pub struct ScriptHost { /* engine */ } // Send + Sync
impl ScriptHost {
    pub fn new(cfg: &SimConfig) -> ScriptHost;
    pub fn compile(&self, source: &str) -> Result<CompiledScript, SimError>; // spec 8.1 の検証。違反は InvalidScript
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarState { pub t: usize, pub position: i8, pub entry_price_milli: i64, pub bars_held: u32 } // t は Dataset::bars() の添字

pub trait Decider {
    /// 1（買い持ち）、-1（売り持ち）、0（持たない）を返す。Err はエラーメッセージ。
    fn on_bar(&mut self, state: BarState) -> Result<i8, String>;
}
pub struct ScriptRun<'a> { /* host, script, params, dataset, this, 要求済みキー, 演算数の合計 */ }
impl<'a> ScriptRun<'a> {
    pub fn new(host: &'a ScriptHost, script: &'a CompiledScript, params: &ParamSet, dataset: std::sync::Arc<Dataset>, cfg: &SimConfig) -> ScriptRun<'a>;
}
impl Decider for ScriptRun<'_>;
```

- [ ] **Step 1:** `crates/sim/scripts/donchian_sar.rhai` を spec 8.1 のスクリプトのとおりに作る。
- [ ] **Step 2:** 登録時の検証の失敗するテストを書く。各ケースが `SimError::InvalidScript` になる。
  - 構文エラー、32 KiB 超、`params` の未定義、`on_bar` の未定義、`params` の引数が 1 個、`on_bar` の引数が 1 個
  - `params()` がマップ以外を返す、要素に `min` がない、整数と小数が混在、`min > default`、`default > max`、`step = 0`、13 個のパラメータ、`default` が刻みの上にない（`min: 10, step: 2, default: 15`）
  - `params()` が無限ループする
- [ ] **Step 3:** 正常な登録の失敗するテストを書く。
  - `donchian_sar.rhai` が登録でき、`params` が `entry`（Int、10〜60、刻み 2、既定 20）の 1 件になる。
  - 同じソースの `sha256` が、`sha2` で直接計算した値と一致する。
  - パラメータが `#{}` のスクリプトが登録できる。
  - トップレベルに `let x = 1;` があるスクリプトが登録でき、実行時にその文が評価されない（`on_bar` が `x` を参照すると実行時エラーになる）。
- [ ] **Step 4:** `resolve_params` の失敗するテストを書く。指定なしが `default` になる。一部指定が残りを `default` で埋める。未知の名前、範囲外、刻み外、整数パラメータへの小数が `SimError::Args` になる。小数パラメータへの JSON の整数が小数として受け付けられる。
- [ ] **Step 5:** 実行の失敗するテストを書く。Task 3 の決定的な系列から `Dataset` を作って使う。
  - `ctx` の各プロパティが `BarState` と足の値から spec 8.2 のとおりに返る（`position`、`entry_price`、`bars_held`、`unrealized_pips`、`time`、`hour`、`weekday`、`spread`）。
  - `ctx.close("M15", 0)` が、足 `t` の時点で完成している最新の M15 の終値を返し、`shift = 1` がその 1 本前を返す。
  - 8 種の指標が、`Dataset::indicator` の系列の、対応する添字の値を返す。`bb` と `keltner` が `upper`・`middle`・`lower`、`donchian` が `upper`・`lower` のキーを持つ。
  - 本数不足で `()` が返る。`()` をそのまま算術に使うスクリプトが `Err` になる。
  - `mult` に整数（`2`）と小数（`2.0`）の両方を渡せ、同じ値が返る。`2.004` と `2.0` が同じキーに丸められる。
  - 実行時エラーになる引数: 未知の `tf`、`period = 0`、`period = 1001`、`shift = -1`、`shift = 1001`、`mult = 0.01`、`mult = 11`、`period` に小数
  - 65 種類目のキーを要求した時点で `Err` になる。64 種類までは成功する。
  - `this` に書いた値が、次の `on_bar` の呼び出しで読める。
  - 戻り値が `2`、`1.0`、`"x"`、`()` の場合に `Err` になる。
  - `on_bar` 内の無限ループが `Err` になる（`max_operations_per_bar` を 10,000 にした設定で確認）。
  - `max_operations_per_run` を小さくした設定で、呼び出しを重ねると途中から `Err` になる。
  - `import "x" as y;`、`sleep(1)`、`timestamp()`、`rand()`、`eval("1")` を使うスクリプトが、登録または実行で失敗する。
  - 2 つの `ScriptRun` を別スレッドで同時に動かしても、演算数の合計と `this` が互いに混ざらない。
- [ ] **Step 6:** `script.rs` を実装し、テストを通す。エンジンの構成は spec 8.3 のとおりとする。
- [ ] **Step 7:** `./scripts/test-all.sh` が `ALL GREEN` であることを確認してコミットする（`feat(sim): add sandboxed Rhai script host`）。

### Task 6: シミュレーション

**Files:**
- Create: `crates/sim/src/engine.rs`

**Interfaces:**
- Consumes: `Dataset`、`Decider`、`BarState`、`SimConfig`、`PIP_MILLI`
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason { Signal, ProtectiveStop, EndOfData }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimTrade {
    pub direction: i8,
    pub entry_idx: usize,   // 評価期間内の添字
    pub exit_idx: usize,
    pub entry_milli: i64,
    pub exit_milli: i64,
    pub pnl_milli: i64,
    pub reason: ExitReason,
}
#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus { Ok, ScriptError { open_time: i64, message: String } }
#[derive(Debug, Clone, PartialEq)]
pub struct SimOutcome {
    pub status: RunStatus,
    pub trades: Vec<SimTrade>,
    pub positions: Vec<i8>, // 評価期間の各足の終値時点のポジション。ScriptError の場合は中断した足まで
}
pub fn simulate(dataset: &Dataset, decider: &mut dyn Decider, cfg: &SimConfig) -> SimOutcome;
```

- [ ] **Step 1:** 失敗するテストを書く。`Decider` は、足ごとの戻り値の列を持つテスト用の実装を使う。`Dataset` は `warmup_bars = 1` で作る。
  - 評価期間の先頭の足では約定しない。先頭の足で `1` を返すと、2 本目の始値の売値（`ask_open`）で買いが建つ。
  - 買いの決済が次の足の `bid_open`、売りの新規が `bid_open`、売りの決済が `ask_open` で行われる。
  - ドテン（`1` の次に `-1`）で、同じ足の始値で決済と新規が行われ、売買が 2 件になり、決済理由が `Signal` になる。
  - `Decider` に渡る `BarState` が、約定と保護ストップを処理した後の状態である。建てた足では `bars_held == 0`、次の足で `1`、ドテン後に `0` へ戻る。
  - 保護ストップ（100 pips）: 買いの建値から `bid_low` が 100 pips 以上下がった足で、`建値 − 1000` ミリ円で決済される。`bid_open` がすでにストップ価格より下の足では `bid_open` で決済される。売りは対称。決済理由が `ProtectiveStop` になり、その足の `positions` が 0 になる。
  - 始値で建てた足の中でストップに達した場合、同じ足で建てて決済する。
  - 保護ストップの直後の足で `Decider` が再び `1` を返すと、その次の足で建て直す。
  - 足が飛ぶ区間（金曜の最後の足の次が月曜の足）をまたいでも、次に存在する足の始値で約定する。
  - 最後の足の戻り値は約定しない。最後の足の後に残ったポジションが、`bid_close`（買い）または `ask_close`（売り）で決済され、理由が `EndOfData` になる。
  - `Decider` が `Err` を返した足で中断し、`RunStatus::ScriptError` の `open_time` がその足の値になり、`trades` と `positions` はそこまでの内容になる。
  - 同じ入力で 2 回実行した結果が等しい。
  - 評価期間が 1 本だけの `Dataset` で panic しない。
- [ ] **Step 2:** `engine.rs` を実装し、テストを通す。処理順は spec 9.2 のとおりとし、価格の計算はすべて `i64` で行う。
- [ ] **Step 3:** `./scripts/test-all.sh` が `ALL GREEN` であることを確認してコミットする（`feat(sim): add bar-by-bar simulation engine`）。

### Task 7: 評価指標

**Files:**
- Create: `crates/sim/src/eval.rs`

**Interfaces:**
- Consumes: `Bar`、`SimOutcome`、`SimTrade`、`Benchmark`、`Leg`
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MissedLeg {
    pub start_time: String,   // RFC 3339（UTC）
    pub end_time: String,
    pub direction: i8,
    pub realizable_pips: f64,
    pub flat_bars: u32,
    pub opposite_bars: u32,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ThetaMetrics {
    pub leg_count: usize,
    pub ideal_pips: f64,
    pub realizable_pips: f64,
    pub capture_rate: Option<f64>,
    pub correct_side_ratio: Option<f64>,
    pub mean_lag_bars: Option<f64>,
    pub mean_lag_pips: Option<f64>,
    pub missed_legs: Vec<MissedLeg>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Metrics {
    pub total_pips: f64,
    pub trade_count: usize,
    pub win_rate: f64,
    pub max_drawdown_pips: f64,
    pub time_in_market: f64,
    pub protective_stop_count: usize,
    pub segments: [f64; 6],
    pub by_theta: std::collections::BTreeMap<String, ThetaMetrics>, // キーは折り返し幅（pips）の 10 進表記
}
impl Metrics {
    pub fn metrics_json(&self) -> serde_json::Value; // spec 10 章の metrics 列の形（segments と by_theta だけ）
}
/// outcome.status が Ok の場合にだけ呼ぶ。
pub fn evaluate(eval_bars: &[Bar], outcome: &SimOutcome, benchmarks: &[Benchmark]) -> Metrics;
```

- [ ] **Step 1:** 失敗するテストを書く。売買、ポジションの列、`Benchmark` を手で組み立てて渡す。
  - 損益が +30、−10、+20 pips の 3 件で、`total_pips = 40`、`trade_count = 3`、`win_rate = 2/3`、`max_drawdown_pips = 10` になる。
  - 最初の売買が −15 pips の場合、最高値の初期値 0 からの下落として `max_drawdown_pips = 15` になる。
  - 売買が 0 件で、`win_rate = 0`、`max_drawdown_pips = 0`、`total_pips = 0` になる。
  - `time_in_market` が、ポジションが 0 でない足の割合になる。
  - `segments`: 評価期間が 13 本なら区間の本数は 2・2・2・2・2・3 で、売買は決済した足の区間に計上される。
  - `capture_rate` が `total_pips / realizable_pips` になる。`realizable_pips` が 0 または負の場合に `None` になる。
  - `correct_side_ratio`: ラベルが付いた 6 本のうち 4 本でポジションが方向と一致する場合に `4/6`。ラベルが 1 本もない場合に `None`。
  - `missed_legs`: 一致した足の割合が 0.5 未満の波だけが入る。ちょうど 0.5 の波は入らない。実質上限の大きい順、同値は開始時刻の早い順に並び、21 件あっても 20 件に切られる。`flat_bars` と `opposite_bars` が spec 10 章の定義どおりである。`start_time` が足 `a+1`、`end_time` が足 `b` の `open_time` である。
  - `mean_lag_bars`: 始点 `a = 10` の波で、最初に一致した足が 13 なら 2（`13 − 11`）。足 `a+1` で一致した場合は 0。一致した足を含む波がない場合に `None`。
  - `mean_lag_pips` が、`mid2_close` の差の絶対値を 20 で割った値の平均になる。
  - `metrics_json()` が spec 10 章の JSON の形（`segments`、`by_theta`、`by_theta` のキーが `"20"` など）になり、`None` が `null` になる。
  - 波が 0 件の `Benchmark` で panic せず、`leg_count = 0`、各 `Option` が `None` になる。
- [ ] **Step 2:** `eval.rs` を実装し、テストを通す。
- [ ] **Step 3:** `./scripts/test-all.sh` が `ALL GREEN` であることを確認してコミットする（`feat(sim): add evaluation metrics`）。

### Task 8: パラメータ探索と保存

**Files:**
- Create: `crates/sim/src/sweep.rs`、`crates/sim/src/store.rs`、`crates/sim/tests/store_test.rs`

**Interfaces:**
- Consumes: `ParamSpec`、`ParamSet`、`ParamValue`、`CompiledScript`、`ScriptHost`、`ScriptRun`、`Dataset`、`simulate`、`evaluate`、`Benchmark`、`RunStatus`、`Metrics`、`SimConfig`、`SimError`
- Produces:

```rust
// sweep.rs
pub fn candidate_count(spec: &ParamSpec) -> u64;
pub fn total_combinations(specs: &[ParamSpec]) -> Result<u128, SimError>; // 桁あふれを検査する。2^63 を超えたら SimError::Args。パラメータ 0 個なら 1
pub fn default_index(specs: &[ParamSpec]) -> Result<u128, SimError>;        // total_combinations と同じ検査を行う
pub fn combination_at(specs: &[ParamSpec], index: u128) -> ParamSet;
pub fn select_indices(specs: &[ParamSpec], max_runs: usize, seed: u64) -> Result<Vec<u128>, SimError>; // 先頭は default_index。total_combinations と同じ検査を行う

#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord { pub params: ParamSet, pub status: RunStatus, pub metrics: Option<Metrics> }
pub fn run_one(dataset: &std::sync::Arc<Dataset>, host: &ScriptHost, script: &CompiledScript, params: &ParamSet, benchmarks: &[Benchmark], cfg: &SimConfig) -> RunRecord;
/// jobs 本のスレッドで実行し、完了したものから on_result を呼ぶ（呼び出しは直列）。
/// on_result が Err を返したら、未着手の実行を中止してその Err を返す。
/// 実行中のスレッドが panic した場合は SimError::BatchFailed を返す。
pub fn run_sweep(
    dataset: &std::sync::Arc<Dataset>, host: &ScriptHost, script: &CompiledScript,
    benchmarks: &[Benchmark], cfg: &SimConfig, indices: &[u128], jobs: usize,
    on_result: &mut (dyn FnMut(RunRecord) -> Result<(), SimError> + Send),
) -> Result<(), SimError>;

// store.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchStatus { Running, Completed, Failed }
pub struct NewBatch { pub script_id: uuid::Uuid, pub period_from: i64, pub period_to: i64, pub bar_count: i32, pub total_runs: i32, pub config: serde_json::Value }
pub async fn register_script(pool: &sqlx::PgPool, name: &str, script: &CompiledScript, origin: &str, parent_id: Option<uuid::Uuid>) -> Result<uuid::Uuid, SimError>;
pub async fn create_batch(pool: &sqlx::PgPool, batch: &NewBatch) -> Result<uuid::Uuid, SimError>;
pub async fn save_run(pool: &sqlx::PgPool, batch_id: uuid::Uuid, record: &RunRecord) -> Result<uuid::Uuid, SimError>;
pub async fn finish_batch(pool: &sqlx::PgPool, batch_id: uuid::Uuid, status: BatchStatus) -> Result<(), SimError>;
pub struct RunSummary { pub params: serde_json::Value, pub total_pips: f64, pub trade_count: i32, pub metrics: serde_json::Value }
pub async fn top_runs(pool: &sqlx::PgPool, batch_id: uuid::Uuid, limit: i64) -> Result<Vec<RunSummary>, SimError>; // completed のバッチだけ。total_pips 降順、同値は params の文字列の昇順
pub fn params_json(params: &ParamSet) -> serde_json::Value;
```

- [ ] **Step 1:** 列挙の失敗するテストを書く。
  - 整数 `min 10, max 60, step 2` の候補が 26 個。小数 `min 0.5, max 2.0, step 0.5` の候補が 4 個で、最後が 2.0 である。小数 `min 0.1, max 0.3, step 0.1` で 0.3 が候補に含まれる（累積誤差の確認）。
  - パラメータ 0 個で `total_combinations == 1`、`combination_at(&[], 0)` が空の `ParamSet` になる。
  - 2 パラメータ（`a` が 3 個、`b` が 4 個）で `total == 12`。`combination_at` の添字 0 が両方 `min`、添字 1 が `b` だけ 1 つ進み、添字 4 が `a` が 1 つ進んだ値になる（辞書順で最後のパラメータが最下位の桁）。
  - `default_index` が `default_params()` と同じ組み合わせを指す。
  - 12 個のパラメータがそれぞれ 1001 個の候補を持つ場合と、12 個の小数パラメータがそれぞれ 10001 個の候補を持つ場合（積が `u128` を超える）に、`total_combinations`、`default_index`、`select_indices` が panic せず `SimError::Args` を返す。9 個のパラメータがそれぞれ 128 個の候補を持つ場合（積がちょうど 2^63）は成功し、そこへ候補 2 個のパラメータを 1 つ足した場合（積が 2^64）は `SimError::Args` になる。
- [ ] **Step 2:** 抽出の失敗するテストを書く。
  - `total <= max_runs` で、全添字が 1 回ずつ返る。先頭が `default_index` である。
  - `total = 27`、`max_runs = 26` で、26 件が返り、重複がなく、先頭が `default_index` である。
  - 同じ `seed` で同じ結果になり、違う `seed` で違う結果になる。
  - `max_runs = 1` で `default_index` だけが返る。
- [ ] **Step 3:** 並列実行の失敗するテストを書く。Task 3 の決定的な系列と `donchian_sar.rhai` を使う。
  - `jobs = 1` と `jobs = 4` で、`RunRecord` の集合（`params` をキーに並べ替えて比較）が一致する。
  - `on_result` が 3 件目で `Err` を返すと、`run_sweep` がその `Err` を返し、`on_result` の呼び出し回数が全件数より少ない。
  - 実行時エラーになるパラメータを含む探索で、その 1 件が `ScriptError`、残りが `Ok` で、`run_sweep` 自体は `Ok` を返す。
- [ ] **Step 4:** `store_test.rs` の失敗するテストを書く（`#[sqlx::test(migrations = "../../migrations")]`）。
  - 同じソースを 2 回 `register_script` すると、同じ `id` が返り、`sim_scripts` が 1 行で、2 回目に渡した `name` で上書きされない。
  - `create_batch` の直後は `status = 'running'`、`finished_at` が NULL。`finish_batch(Completed)` で `completed` になり、`finished_at` が入る。`Failed` も同様。
  - `save_run` で、`Ok` の行は指標の列と `metrics` が入り、`error` が NULL。`ScriptError` の行は `error` が `<RFC 3339> <メッセージ>` の形で、指標の列と `metrics` が NULL。
  - 同じ `batch_id` と `params` の `save_run` が 2 回目でエラーになる。
  - `top_runs` が、`running` と `failed` のバッチでは空を返し、`completed` のバッチでは `total_pips` の降順、同値は `params` の文字列の昇順で返す。
- [ ] **Step 5:** `sweep.rs` と `store.rs` を実装し、テストを通す。抽出は spec 11 章の `rand::seq::index::sample` と `ChaCha8Rng::seed_from_u64` を使う。
- [ ] **Step 6:** `./scripts/test-all.sh` が `ALL GREEN` であることを確認してコミットする（`feat(sim): add parameter sweep and result storage`）。

### Task 9: コマンド

**Files:**
- Create: `crates/sim/src/cli.rs`、`crates/sim/tests/cli_test.rs`
- Modify: `crates/sim/src/main.rs`、`crates/sim/src/lib.rs`

**Interfaces:**
- Consumes: これまでのすべての公開 API
- Produces:

```rust
// cli.rs
#[derive(clap::Parser)]
pub struct Cli { #[command(subcommand)] pub command: Command }
#[derive(clap::Subcommand)]
pub enum Command { Migrate, Backfill { .. }, Benchmark { .. }, Run { .. }, Sweep { .. } } // 引数は spec 13 章
/// 終了コードを返す。設定の読み込みから結果の出力までを行う。
pub async fn run(cli: Cli) -> i32;
/// --from / --to の省略時の規則（spec 13 章）を適用して、評価期間 [from, to) をエポック秒で返す。
pub fn resolve_period(all_bars: &[Bar], from: Option<chrono::NaiveDate>, to: Option<chrono::NaiveDate>, warmup_bars: usize) -> Result<(i64, i64), SimError>;
```

処理の順序（`run` と `sweep`）: 設定の読み込みと検証 → 引数の検証 → DB 接続と `ensure_tables` → スクリプトの読み込みと `compile` → 足の読み込みと `Dataset::new` → `missing_weekdays` の WARN → 基準値の計算 → `register_script` → `create_batch` → 実行と `save_run` → `finish_batch` → 出力。バッチの行を作った後の失敗は、すべて `finish_batch(Failed)` を試みてから終了コード 1 を返す。`finish_batch(Failed)` 自体が失敗した場合は ERROR で記録する。

- [ ] **Step 1:** `resolve_period` の失敗する単体テストを書く。
  - `from` 省略時に、`warmup_bars` 本目の足の `open_time` 以降で最初の UTC 0:00 が返る（その足がちょうど 0:00 なら同じ時刻）。
  - `to` 省略時に、最後の足の UTC の日付の翌日 0:00 が返る。
  - `from >= to` が `SimError::Args` になる。足が 0 本の場合に `SimError::Args` になる。
- [ ] **Step 2:** `cli_test.rs` の失敗するテストを書く（`#[sqlx::test(migrations = "../../migrations")]`。設定は一時ファイルに書き、`CONFIG_PATH` ではなく `cli::run` に設定のパスを渡せる内部関数を用意して使う）。Task 3 の決定的な系列を `upsert_bars` で投入する。
  - `benchmark`: 終了コード 0。`--json` の出力に、`thetas_pips` の各値の `leg_count`、`ideal_pips`、`realizable_pips` がある。
  - `run`（`donchian_sar.rhai`、`--params` 省略）: 終了コード 0。`sim_batches` が 1 行（`completed`、`total_runs = 1`、`bar_count` が評価期間の本数、`config` に `warmup_bars`・`protective_stop_pips`・`thetas_pips`・`max_operations_per_bar`・`max_operations_per_run`）、`sim_runs` が 1 行、`sim_scripts` の `name` が `donchian_sar`、`origin` が `human`。
  - `run --params '{"entry": 30}'` が成功する。`'{"entry": 31}'`（刻み外）と `'{"x": 1}'`（未知の名前）が終了コード 1 で、`sim_batches` に行を作らない。
  - 実行時エラーになるスクリプトの `run`: 終了コード 1。`sim_runs` に `script_error` の行が保存され、バッチは `completed` になる。
  - 構文エラーのスクリプトの `run`: 終了コード 1。`sim_scripts` と `sim_batches` に行を作らない。
  - `sweep --max-runs 5 --jobs 2`: 終了コード 0。`sim_runs` が 5 行。`--json` の出力の上位件数が 5 以下で、`total_pips` の降順である。
  - `sweep` を `--max-runs` なし、`--max-runs 0`、`--max-runs 1000001` で実行すると終了コード 1（clap のエラーを含む）。
  - `backfill`（wiremock）: 1 日だけ失敗させると終了コード 1、成功だけなら 0。
  - `sim_runs` テーブルを DROP した DB で `run` を実行すると、終了コード 1 で、バッチを作らない。
  - 検証違反の設定（`jobs = 0`）で、どのサブコマンドも終了コード 1。
- [ ] **Step 3:** `cli.rs` と `main.rs` を実装し、テストを通す。`migrate` は `sqlx::migrate!("../../migrations")` を実行する。人間向けの出力は表形式、`--json` は 1 つの JSON オブジェクトとする。所要時間は秒（小数 2 桁）で出力する。ログは `tracing` で標準エラーへ出し、結果は標準出力へ出す。
- [ ] **Step 4:** `./scripts/test-all.sh` が `ALL GREEN` であることを確認してコミットする（`feat(sim): add auto-trader-sim CLI`）。

---

## 実行の単位

vibepod の 1 回の実行を短く保つため、次の 7 回に分けて順に実行する。各回で kaneko が実装し、reviewer が Stage 1〜2 を PASS または CONDITIONAL PASS まで通す。

| 回 | タスク |
| --- | --- |
| 1 | Task 1、Task 2 |
| 2 | Task 3 |
| 3 | Task 4 |
| 4 | Task 5 |
| 5 | Task 6、Task 7 |
| 6 | Task 8 |
| 7 | Task 9 |

各回の完了後、オーケストレーターがホストで `./scripts/test-all.sh` を再実行し、差分と reviewer の判定を確認してから次の回へ進む。

## 受け入れ確認

Task 9 の完了後、オーケストレーターが開発機で spec 15 章の 2〜5 を実行し、結果（保存された足の本数、各折り返し幅の波の数と基準値、`run` の所要時間、`sweep` の `--jobs 1` と `--jobs 4` の一致）を記録して報告する。
