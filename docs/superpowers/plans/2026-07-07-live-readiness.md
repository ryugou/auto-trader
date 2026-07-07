# Live Readiness (本番運用準備) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 2026-07-07 監査で確定した live 切り替えブロッカー 6 件を解消し、paper→live 切り替えを安全に実行できる状態にする。

**Architecture:** 既存の単一 `Trader`（dry_run 分岐は fill のみ）という paper=live 統合設計は維持する。追加するのは (1) エントリーゲートの拡充（Kill Switch）、(2) サイジングの安全バッファ、(3) live 専用の監視系（維持率アラート・残高照合・取引所側 SL）、(4) 検証基盤（backtest 近代化・regime 窓長修正）。**paper/live で挙動が分かれる箇所は必ず「取引所が live で自動的にやること」の paper 代行に限定する**（既存の swap/SFD/ロスカットと同じ原則）。

**Tech Stack:** Rust (edition 2024) / tokio / sqlx (PostgreSQL) / rust_decimal / 既存 workspace crates（core, db, market, executor, app, notify, backtest）

---

## 実装者への必須ルール（全 Phase 共通）

このリポジトリには厳守ルールがある。**各 Phase の実装前に必ず読むこと**:

1. **Phase ごとに 1 ブランチ + 1 PR**。main 直接 push は絶対禁止（git hook で拒否される）。ブランチ名は各 Phase 冒頭に記載。
2. **commit 前に必ず `./scripts/test-all.sh` を実行し、最後に `ALL GREEN` が出ることを確認する**。個別に `cargo test` だけ流して commit するのは禁止（開発中の反復では個別テスト実行して良いが、commit 直前は必ず test-all.sh）。DB は docker-compose の Postgres (localhost:15432) が自動起動される。
3. コミットメッセージは Conventional Commits（`feat:` / `fix:` / `test:` / `docs:`）。
4. **push/PR 前に `code-review` スキルのフローを実行する**（Claude Code 環境の場合）。
5. TDD で進める: 失敗するテストを先に書き、落ちることを確認してから実装する。
6. 各 Phase 完了時に `specs/` 配下の該当ドキュメントを更新する（各 Phase 最終タスクに記載）。
7. **金額はすべて `rust_decimal::Decimal`**。f64 を金額計算に使わない。円建て台帳への書き込みは既存の `truncate_yen`（`crates/executor/src/trader.rs:37`）の規約に従う。
8. 対象取引所は **bitFlyer Crypto CFD と GMO Coin FX のみ**。OANDA 関連コード（enum 残置・`oanda_private.rs` 等）には触れない・言及しない。
9. 外部 API（bitFlyer / GMO）の request/response 形は、**実装前に必ず公式ドキュメントで最新仕様を確認する**（Phase 4 の Task 4.0 参照）。本計画のコードは執筆時点の仕様理解に基づく叩き台であり、フィールド名の食い違いがあれば公式 doc を正とする。

### Phase の依存関係と実行順

```
Phase 1 (EUR_USD 除外)          — 独立・最優先。即日可能
Phase 2 (Kill Switch)           — 独立
Phase 3 (サイジングバッファ + 維持率アラート) — Phase 2 の後推奨 (main.rs の同じ領域を触るため)
Phase 4 (取引所側 SL)           — Phase 3 の後 (PositionSizer コンストラクタを両方が変更)
Phase 5 (残高照合)              — Phase 3 の後 (NotifyEvent::SystemAlert を使う)
Phase 6 (regime 修正 + backtest) — 独立。他 Phase と並行可
```

conflict を避けるため、**Phase 1→2→3→4→5 は直列で実施**すること。Phase 6 のみ並行可。

---

## 背景（現状の要点、監査 2026-07-07 より）

- paper/live は単一 `Trader`（`crates/executor/src/trader.rs`）で、`dry_run` による分岐は `fill_open`(L183) / `fill_close`(L382) のみ。DB 書き込み・margin lock・PnL 計算は完全共通。
- エントリーゲートは価格鮮度チェック 1 つだけ（`crates/executor/src/risk_gate.rs`）＋「同一戦略×ペアの重複ポジション防止」（`crates/app/src/main.rs:1414`）。
- サイジングは `PositionSizer`（`crates/executor/src/position_sizer.rs`）が「SL ヒット時に維持率がちょうどロスカット閾値 Y に着地する最大量」を張る。バッファなし。
- SL/TP はアプリの tick 監視（`main.rs:957-968`）のみ。取引所側にストップ注文は置かれない。
- ロスカット監視（`crates/app/src/liquidation.rs`）は paper 限定。
- live 残高は DB 管理（initial + Σpnl − Σfees）で、取引所実残高との照合機構なし。
- config 上 `EUR_USD` が有効だが、`position_sizer.rs:84` と `core/src/margin.rs:46` は JPY quote 前提で、quote=USD ペアでは証拠金を誤算する。
- backtest crate（`crates/backtest/src/runner.rs`）は oanda ハードコード・quantity=1 固定・`pnl = price_diff × leverage`（数量非考慮のバグ）・`on_open_positions`（戦略 exit）未再生で、現行戦略の検証に使えない。

---

# Phase 1: EUR_USD 除外 + JPY-quote ガード

**ブランチ:** `fix/drop-eur-usd`
**目的:** 通貨単位バグ（ブロッカー #1）を「非 JPY quote ペアを構成レベルで拒否する」ことで解消する。cross-currency 換算は実装しない（将来必要になったら別計画）。

### Task 1.1: 設定バリデーション — 非 JPY quote ペアを起動時拒否

**Files:**
- Modify: `crates/core/src/config.rs`（`AppConfig::validate`, L224 付近）
- Test: 同ファイル内 `#[cfg(test)]`

- [ ] **Step 1: 失敗するテストを書く**

`crates/core/src/config.rs` のテストモジュール（既存の `debug_redaction_tests` の近く）に追加:

```rust
#[cfg(test)]
mod jpy_quote_validation_tests {
    use super::*;

    fn base_toml(pairs_fx: &str, strategy_pairs: &str) -> String {
        format!(
            r#"
[vegapunk]
endpoint = "http://localhost:6840"
schema = "fx-trading"

[database]
url = "postgresql://localhost/test"

[monitor]
interval_secs = 60

[pairs]
fx = {pairs_fx}
crypto = ["FX_BTC_JPY"]

[[strategies]]
name = "donchian_trend_v1"
enabled = true
mode = "paper"
pairs = {strategy_pairs}
"#
        )
    }

    #[test]
    fn rejects_non_jpy_quote_pair_in_pairs_fx() {
        let toml_str = base_toml(r#"["USD_JPY", "EUR_USD"]"#, r#"["USD_JPY"]"#);
        let config: AppConfig = toml::from_str(&toml_str).unwrap();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("EUR_USD"), "error should name the pair: {err}");
    }

    #[test]
    fn rejects_non_jpy_quote_pair_in_strategy_pairs() {
        let toml_str = base_toml(r#"["USD_JPY"]"#, r#"["EUR_USD"]"#);
        let config: AppConfig = toml::from_str(&toml_str).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_jpy_quote_pairs() {
        let toml_str = base_toml(r#"["USD_JPY"]"#, r#"["USD_JPY", "FX_BTC_JPY"]"#);
        let config: AppConfig = toml::from_str(&toml_str).unwrap();
        assert!(config.validate().is_ok());
    }
}
```

注: `base_toml` の必須セクション（vegapunk/database/monitor）は `AppConfig` の非 Option フィールド（config.rs:7-29）に合わせてある。コンパイルエラーが出たら実際の必須フィールドに合わせて調整すること。

- [ ] **Step 2: テストが落ちることを確認**

Run: `cargo test -p auto-trader-core jpy_quote -- --nocapture`
Expected: FAIL（validate がまだ EUR_USD を通すため assertion 失敗）

- [ ] **Step 3: バリデーションを実装**

`crates/core/src/config.rs` のモジュールレベル（`impl AppConfig` の外）に追加:

```rust
/// 口座は全て JPY 建て。quote 通貨が JPY でないペア (EUR_USD 等) は、
/// position_sizer / margin が price×qty を JPY 金額として扱う前提と矛盾し
/// 証拠金・維持率を誤算するため、起動時に拒否する。
/// cross-currency 換算を実装するまでこのガードを外してはならない。
fn ensure_jpy_quote(pairs: &[String], section: &str) -> anyhow::Result<()> {
    for p in pairs {
        if !p.ends_with("_JPY") {
            anyhow::bail!(
                "[{section}] pair '{p}' is not JPY-quoted; \
                 non-JPY quote pairs are unsupported (margin math assumes price×qty is JPY)"
            );
        }
    }
    Ok(())
}
```

`AppConfig::validate()`（L224）の既存チェックの後に追加:

```rust
ensure_jpy_quote(&self.pairs.fx, "pairs.fx")?;
if let Some(crypto) = &self.pairs.crypto {
    ensure_jpy_quote(crypto, "pairs.crypto")?;
}
ensure_jpy_quote(&self.pairs.active, "pairs.active")?;
for s in &self.strategies {
    ensure_jpy_quote(&s.pairs, &format!("strategies({})", s.name))?;
}
```

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p auto-trader-core jpy_quote`
Expected: PASS (3 tests)

### Task 1.2: config / fixture から EUR_USD を除去

**Files:**
- Modify: `config/default.toml`
- Modify: grep で見つかる fixture / テスト

- [ ] **Step 1: EUR_USD の全参照を洗い出す**

Run: `grep -rn "EUR_USD" config/ crates/ migrations/ specs/ --include="*.toml" --include="*.rs" --include="*.sql" --include="*.md" | grep -v target`

- [ ] **Step 2: config/default.toml を修正**

- L17: `fx = ["USD_JPY", "EUR_USD"]` → `fx = ["USD_JPY"]`
- L32-34: `[pair_config.EUR_USD]` セクションを削除
- L81: swing_llm_v1 の `pairs = ["USD_JPY", "EUR_USD"]` → `pairs = ["USD_JPY"]`

- [ ] **Step 3: Step 1 で見つかった他の参照を処理**

方針: **テスト fixture が「EUR_USD が有効であること」を前提にしている場合はテストを「EUR_USD が拒否されること」の検証に書き換える**。単にペアリストに含めているだけなら USD_JPY に置換。migration に EUR_USD の trading_accounts 行を seed しているものがあれば、新 migration で該当行を削除する（既存 migration ファイルは変更しない）。

- [ ] **Step 4: 全体テスト**

Run: `./scripts/test-all.sh`
Expected: `ALL GREEN`

- [ ] **Step 5: specs 更新 + commit**

`specs/design.md` の通貨ペアの節（L37-41）に「非 JPY quote ペアは config validation で拒否される（cross-currency 未対応のため）」を追記。

```bash
git add -A
git commit -m "fix(config): drop EUR_USD and reject non-JPY-quote pairs at startup"
```

PR を作成し、レビューフローを実行する。

---

# Phase 2: dry_run 判定の一元化 + Kill Switch（日次損失上限）

**ブランチ:** `feat/kill-switch`
**目的:** ブロッカー #2。当日（JST）実現損失が当日開始残高の一定割合（デフォルト 5%）を超えた口座を一定時間（デフォルト 24h）新規エントリー禁止にする。**paper/live 共通に適用**（paper=live 原則。paper でも検証データが歪むのを防ぐ意味がある）。あわせて 4 箇所に複製されている dry_run 判定式をヘルパに集約する（保守性）。

### Task 2.1: dry_run 判定ヘルパ

**Files:**
- Modify: `crates/app/src/startup.rs`
- Modify: `crates/app/src/main.rs`（L946 付近, L1184 付近, L1343）、`crates/app/src/liquidation.rs:73`

- [ ] **Step 1: ヘルパを追加**

`crates/app/src/startup.rs` に追加:

```rust
/// paper/live 判定の唯一の定義。`account_type == "paper"` もしくは
/// LIVE_DRY_RUN 強制時に dry_run。この式を呼び出し側にコピーしないこと
/// (判定が分岐すると paper=live 契約が壊れる)。
pub fn effective_dry_run(account_type: &str, live_forces_dry_run: bool) -> bool {
    account_type == "paper" || live_forces_dry_run
}
```

- [ ] **Step 2: 全複製箇所を置換**

Run: `grep -rn 'account_type == "paper" ||' crates/app/src --include="*.rs"`

ヒットした各行（main.rs:1343、main.rs の SL/TP close 経路、strategy exit 経路、liquidation.rs:73）を `startup::effective_dry_run(...)` 呼び出しに置換する。**式の意味は変えない**（純粋な抽出リファクタ）。

- [ ] **Step 3: テスト実行 + commit**

Run: `cargo test -p auto-trader-app && cargo clippy -p auto-trader-app -- -D warnings`
Expected: PASS

```bash
git add -A
git commit -m "refactor(app): consolidate dry_run derivation into startup::effective_dry_run"
```

### Task 2.2: halt カラムの migration と DB 関数

**Files:**
- Create: `migrations/20260708000001_add_account_halt.sql`
- Modify: `crates/db/src/trading_accounts.rs`
- Modify: `crates/db/src/trades.rs`

- [ ] **Step 1: migration を作成**

```sql
-- Kill Switch: 日次損失上限を超えた口座の新規エントリー停止。
-- halted_until が未来の間、entry signal は拒否される。close は常に許可。
ALTER TABLE trading_accounts
    ADD COLUMN halted_until TIMESTAMPTZ,
    ADD COLUMN halt_reason TEXT;
```

- [ ] **Step 2: 失敗するテストを書く（DB 関数）**

`crates/db/src/trading_accounts.rs` のテストに追加（既存テストの `#[sqlx::test]` パターンを踏襲。既存に sqlx::test が無ければ `crates/db/tests/migration_test.rs` のセットアップ方式に合わせる）:

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn set_and_get_halt_roundtrip(pool: sqlx::PgPool) {
    // 既存 seed 口座を 1 件取得 (list_all の先頭)。無ければテスト内で create_account。
    let accounts = list_all(&pool).await.unwrap();
    let id = accounts[0].id;

    assert!(get_halt(&pool, id).await.unwrap().is_none());

    let until = chrono::Utc::now() + chrono::Duration::hours(24);
    set_halt(&pool, id, until, "daily loss limit").await.unwrap();

    let (got_until, reason) = get_halt(&pool, id).await.unwrap().unwrap();
    assert_eq!(got_until.timestamp(), until.timestamp());
    assert_eq!(reason.as_deref(), Some("daily loss limit"));
}
```

Run: `cargo test -p auto-trader-db set_and_get_halt` → Expected: コンパイルエラー（関数未定義）

- [ ] **Step 3: DB 関数を実装**

`crates/db/src/trading_accounts.rs` に追加:

```rust
/// Kill Switch の halt 状態を読む。halted_until が NULL なら None。
pub async fn get_halt(
    pool: &PgPool,
    id: Uuid,
) -> anyhow::Result<Option<(DateTime<Utc>, Option<String>)>> {
    let row: Option<(Option<DateTime<Utc>>, Option<String>)> = sqlx::query_as(
        "SELECT halted_until, halt_reason FROM trading_accounts WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(until, reason)| until.map(|u| (u, reason))))
}

/// Kill Switch を作動させる。解除は halted_until 経過を待つか、運用者が
/// SQL で halted_until を NULL にする (specs/design.md 運用手順参照)。
pub async fn set_halt(
    pool: &PgPool,
    id: Uuid,
    until: DateTime<Utc>,
    reason: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE trading_accounts SET halted_until = $2, halt_reason = $3 WHERE id = $1")
        .bind(id)
        .bind(until)
        .bind(reason)
        .execute(pool)
        .await?;
    Ok(())
}
```

`crates/db/src/trades.rs` に追加:

```rust
/// account の `since` 以降にクローズした trade の実現損益合計 (pnl_amount - fees)。
/// Kill Switch の日次損失判定に使う。open 中の含み損・open 中 trade への
/// swap/SFD 累積は含まない (保守的でない近似だが、実現ベースで一貫)。
pub async fn realized_net_since(
    pool: &PgPool,
    account_id: Uuid,
    since: DateTime<Utc>,
) -> anyhow::Result<Decimal> {
    let row: (Option<Decimal>,) = sqlx::query_as(
        "SELECT SUM(pnl_amount - fees) FROM trades \
         WHERE account_id = $1 AND status = 'closed' AND exit_at >= $2",
    )
    .bind(account_id)
    .bind(since)
    .fetch_one(pool)
    .await?;
    Ok(row.0.unwrap_or(Decimal::ZERO))
}
```

`realized_net_since` にも同様の `#[sqlx::test]`（closed trade を 2 件 insert して合計を検証、boundary: since より前の trade は含まれない）を書くこと。trade の insert は `insert_trade` + `update_trade_closed` を使う。

- [ ] **Step 4: テストが通ることを確認 + commit**

Run: `cargo test -p auto-trader-db`
Expected: PASS

```bash
git add -A
git commit -m "feat(db): account halt columns + realized_net_since for kill switch"
```

### Task 2.3: risk_gate に日次損失判定を追加

**Files:**
- Modify: `crates/executor/src/risk_gate.rs`
- Modify: `crates/core/src/config.rs`（RiskConfig, L184-196）

- [ ] **Step 1: 失敗するテストを書く**

`crates/executor/src/risk_gate.rs` に追加:

```rust
#[cfg(test)]
mod daily_loss_tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    #[test]
    fn jst_day_start_is_15h_utc_of_previous_day() {
        // 2026-07-07 10:00 JST = 2026-07-07 01:00 UTC → JST 当日 00:00 = 07-06 15:00 UTC
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 7, 1, 0, 0).unwrap();
        let start = jst_day_start(now);
        assert_eq!(start, chrono::Utc.with_ymd_and_hms(2026, 7, 6, 15, 0, 0).unwrap());
        // 2026-07-07 23:00 UTC = 07-08 08:00 JST → JST 当日 00:00 = 07-07 15:00 UTC
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 7, 23, 0, 0).unwrap();
        assert_eq!(
            jst_day_start(now),
            chrono::Utc.with_ymd_and_hms(2026, 7, 7, 15, 0, 0).unwrap()
        );
    }

    #[test]
    fn rejects_at_exactly_limit_and_beyond() {
        // 開始残高 30,000, limit 5% → -1,500 ちょうどで発火 (<= 比較)
        assert!(matches!(
            eval_daily_loss(dec!(-1500), dec!(30000), dec!(0.05)),
            GateDecision::Reject(RejectReason::DailyLossLimit { .. })
        ));
        assert!(matches!(
            eval_daily_loss(dec!(-1501), dec!(30000), dec!(0.05)),
            GateDecision::Reject(_)
        ));
    }

    #[test]
    fn passes_below_limit_and_on_profit() {
        assert!(matches!(
            eval_daily_loss(dec!(-1499), dec!(30000), dec!(0.05)),
            GateDecision::Pass
        ));
        assert!(matches!(
            eval_daily_loss(dec!(500), dec!(30000), dec!(0.05)),
            GateDecision::Pass
        ));
    }

    #[test]
    fn passes_when_day_start_balance_non_positive() {
        // 開始残高 0 以下は判定不能 → Pass (エントリー自体は sizing で弾かれる)
        assert!(matches!(
            eval_daily_loss(dec!(-100), dec!(0), dec!(0.05)),
            GateDecision::Pass
        ));
    }
}
```

Run: `cargo test -p auto-trader-executor daily_loss` → Expected: コンパイルエラー

- [ ] **Step 2: 実装**

`crates/executor/src/risk_gate.rs` に追加（既存 enum を拡張）:

```rust
use chrono::{DateTime, Duration, TimeZone, Utc};
use rust_decimal::Decimal;

#[derive(Debug, Clone)]
pub enum RejectReason {
    PriceTickStale { age_secs: u64 },
    DailyLossLimit { day_net: Decimal, limit_amount: Decimal },
    Halted { until: DateTime<Utc> },
}

impl RejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PriceTickStale { .. } => "price_tick_stale",
            Self::DailyLossLimit { .. } => "daily_loss_limit",
            Self::Halted { .. } => "halted",
        }
    }
}

/// JST (UTC+9) の当日 00:00 を UTC で返す。Kill Switch の日次区切り。
pub fn jst_day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let jst = now + Duration::hours(9);
    let day_start_jst = jst.date_naive().and_hms_opt(0, 0, 0).expect("00:00:00 is valid");
    Utc.from_utc_datetime(&(day_start_jst - Duration::hours(9)))
}

/// 日次損失上限の判定。pure 関数。
///
/// - `day_net`: 当日 (JST) にクローズした trade の Σ(pnl_amount - fees)
/// - `day_start_balance`: 当日開始時点の残高 (= current_balance - day_net)
/// - `limit_pct`: 上限割合 (0.05 = 5%)
///
/// `day_net <= -(day_start_balance × limit_pct)` で Reject。
/// day_start_balance <= 0 は判定不能なので Pass (sizing 側で弾かれる)。
pub fn eval_daily_loss(
    day_net: Decimal,
    day_start_balance: Decimal,
    limit_pct: Decimal,
) -> GateDecision {
    if day_start_balance <= Decimal::ZERO {
        return GateDecision::Pass;
    }
    let limit_amount = day_start_balance * limit_pct;
    if day_net <= -limit_amount {
        GateDecision::Reject(RejectReason::DailyLossLimit { day_net, limit_amount })
    } else {
        GateDecision::Pass
    }
}
```

注: `Halted` variant はこのタスクでは main.rs 側から直接 warn ログに使う（Task 2.4）。executor crate の Cargo.toml に `chrono` が無ければ workspace 依存から追加する。

`crates/core/src/config.rs` の RiskConfig を拡張:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    pub price_freshness_secs: u64,
    /// Kill Switch: 当日 (JST) 実現損失の上限 (当日開始残高比)。
    /// 超過した口座は halt_hours の間、新規エントリー禁止。close は許可。
    #[serde(default = "default_daily_loss_limit_pct")]
    pub daily_loss_limit_pct: Decimal,
    #[serde(default = "default_halt_hours")]
    pub halt_hours: u64,
}

fn default_daily_loss_limit_pct() -> Decimal {
    Decimal::new(5, 2) // 0.05
}

fn default_halt_hours() -> u64 {
    24
}
```

validate に追加:

```rust
if self.daily_loss_limit_pct <= Decimal::ZERO || self.daily_loss_limit_pct >= Decimal::ONE {
    anyhow::bail!("[risk].daily_loss_limit_pct must be in (0, 1)");
}
if self.halt_hours == 0 {
    anyhow::bail!("[risk].halt_hours must be > 0");
}
```

`config/default.toml` の `[risk]` に明示値を追記:

```toml
[risk]
price_freshness_secs = 60
daily_loss_limit_pct = 0.05  # Kill Switch: 当日実現損失が開始残高の5%で24h新規停止
halt_hours = 24
```

- [ ] **Step 3: テスト + commit**

Run: `cargo test -p auto-trader-executor -p auto-trader-core`
Expected: PASS

```bash
git add -A
git commit -m "feat(executor): daily loss kill-switch gate (pure eval) + risk config"
```

### Task 2.4: エントリー経路への配線

**Files:**
- Modify: `crates/app/src/main.rs`（エントリー signal 処理、L1332-1427 付近）

- [ ] **Step 1: 配線コードを追加**

main.rs のエントリー処理で、`[live].enabled` チェック（L1335-1342）の直後・`dry_run` 計算の前に挿入:

```rust
// --- Kill Switch: halted 口座は新規発注しない (close 経路には影響しない) ---
match auto_trader_db::trading_accounts::get_halt(&executor_pool, pac.id).await {
    Ok(Some((until, reason))) if until > chrono::Utc::now() => {
        tracing::warn!(
            "kill switch: account {} halted until {} ({}); skipping entry signal",
            pac.name,
            until,
            reason.as_deref().unwrap_or("-")
        );
        continue;
    }
    Ok(_) => {}
    Err(e) => {
        // fail-closed: halt 状態が読めないなら発注しない
        tracing::warn!(
            "kill switch: failed to read halt state for {}: {e}; skipping (fail-closed)",
            pac.name
        );
        continue;
    }
}

// --- 日次損失上限。超過していたら halt を刻んで skip ---
let day_start = auto_trader_executor::risk_gate::jst_day_start(chrono::Utc::now());
let day_net = match auto_trader_db::trades::realized_net_since(&executor_pool, pac.id, day_start)
    .await
{
    Ok(v) => v,
    Err(e) => {
        tracing::warn!(
            "daily loss check failed for {}: {e}; skipping (fail-closed)",
            pac.name
        );
        continue;
    }
};
let day_start_balance = pac.current_balance - day_net;
if let GateDecision::Reject(reason) = auto_trader_executor::risk_gate::eval_daily_loss(
    day_net,
    day_start_balance,
    executor_daily_loss_limit_pct,
) {
    let until = chrono::Utc::now() + chrono::Duration::hours(executor_halt_hours as i64);
    let reason_text = format!(
        "daily loss limit: day_net={day_net} (limit={}% of day-start balance {day_start_balance})",
        executor_daily_loss_limit_pct * rust_decimal::Decimal::from(100)
    );
    if let Err(e) = auto_trader_db::trading_accounts::set_halt(
        &executor_pool,
        pac.id,
        until,
        &reason_text,
    )
    .await
    {
        tracing::error!("kill switch: set_halt failed for {}: {e}", pac.name);
    }
    tracing::warn!(
        "kill switch TRIGGERED for account {}: {:?}; halted until {until}",
        pac.name,
        reason
    );
    // Slack 通知 (既存 OrderFailed パターンを踏襲, fire-and-forget)
    let notifier = executor_notifier.clone();
    let ev = auto_trader_notify::NotifyEvent::OrderFailed(auto_trader_notify::OrderFailedEvent {
        account_name: pac.name.clone(),
        exchange,
        strategy_name: signal.strategy_name.clone(),
        pair: signal.pair.clone(),
        reason: format!("KILL SWITCH: {reason_text}; halted until {until}"),
    });
    tokio::spawn(async move {
        if let Err(e) = notifier.send(ev).await {
            tracing::warn!("kill switch notify failed: {e}");
        }
    });
    continue;
}
```

`executor_daily_loss_limit_pct` / `executor_halt_hours` は、`executor_price_freshness_secs` が config から clone されている箇所（main.rs 内で `price_freshness_secs` を検索）と同じ方法で config から取り出して executor タスクに渡す。

注意: `pac` に `current_balance` が無い（口座リストが軽量 struct の）場合は `get_account` で読み直すこと。既存コード L756（`Trader::execute` 内）と同じ関数が使える。

- [ ] **Step 2: 統合テストを書く**

`crates/integration-tests/tests/phase3_guards.rs` の既存テスト構成（helpers::pipeline / seed の使い方）を読み、同じパターンで追加する。検証内容:

1. 口座を seed し、当日 exit の closed trade（pnl_amount = 開始残高の -6% 相当, fees=0）を insert
2. entry signal を流す
3. trade が **開かれない** こと、`trading_accounts.halted_until` が未来時刻でセットされたことを assert
4. 2 発目の signal も halted でスキップされること

さらに boundary テスト: 損失 -4% では trade が開かれること。

- [ ] **Step 3: 全体テスト + commit + PR**

Run: `./scripts/test-all.sh`
Expected: `ALL GREEN`

```bash
git add -A
git commit -m "feat(app): wire kill switch into entry path (halt check + daily loss eval)"
```

specs 更新: `specs/design.md` に Kill Switch の仕様（JST 日次区切り・実現ベース・fail-closed・手動解除は `UPDATE trading_accounts SET halted_until = NULL WHERE name = '...'`）を追記して PR 作成。

---

# Phase 3: サイジング安全バッファ + live 維持率アラート

**ブランチ:** `feat/sizing-buffer-margin-alerts`
**目的:** ブロッカー #3。(a) PositionSizer の「SL ヒット = 維持率ちょうど Y」設計にバッファを入れ、スリッページ/ギャップで即ロスカットになる構造を解消。(b) live 口座の維持率を bot 側でも監視し、閾値接近を Slack 警告（自動クローズはしない — live のロスカット執行は取引所の責務のまま）。

### Task 3.1: PositionSizer に margin_buffer を追加

**Files:**
- Modify: `crates/executor/src/position_sizer.rs`
- Modify: PositionSizer::new の全呼び出し元（`grep -rn "PositionSizer::new" crates/ --include="*.rs" | grep -v target` で列挙）
- Modify: `crates/core/src/config.rs`（RiskConfig）

- [ ] **Step 1: 失敗するテストを書く**

`position_sizer.rs` テストに追加（既存テストはまず `PositionSizer::new(min_sizes, Decimal::ZERO)` に書き換えて従来値を維持する）:

```rust
/// buffer=0.10: gmo_fx (Y=1.0) lev=10, SL=2% →
/// max_alloc = 1/(1.0+0.10+0.2) = 0.7692...
/// 30,000 × 10 × 0.7692 / 157 = 1469.9... → 1469 (min_lot=1)
#[test]
fn margin_buffer_tightens_allocation() {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let sizer = PositionSizer::new(min_sizes, dec!(0.10));
    let qty = sizer.calculate_quantity(
        &Pair::new("USD_JPY"),
        dec!(30000),
        dec!(157),
        dec!(10),
        dec!(1.0),
        dec!(0.02),
        dec!(1.00),
    );
    assert_eq!(qty, Some(dec!(1469)));
}
```

Run: `cargo test -p auto-trader-executor margin_buffer` → Expected: コンパイルエラー

- [ ] **Step 2: 実装**

```rust
pub struct PositionSizer {
    min_order_sizes: HashMap<Pair, Decimal>,
    /// Y に上乗せする安全バッファ。max_alloc = 1 / (Y + buffer + L×s)。
    /// スリッページ・週末ギャップ・SL 発動遅延で実現損失が SL 価格を
    /// 超過しても、維持率がロスカット閾値まで即落ちしないための余裕。
    margin_buffer: Decimal,
}

impl PositionSizer {
    pub fn new(min_order_sizes: HashMap<Pair, Decimal>, margin_buffer: Decimal) -> Self {
        Self { min_order_sizes, margin_buffer }
    }
    // calculate_quantity 内:
    //   let y_eff = liquidation_margin_level + self.margin_buffer;
    //   let max_alloc = Decimal::ONE / (y_eff + leverage * stop_loss_pct);
}
```

doc コメント（L5-26）の式説明も buffer 込みに更新する。

- [ ] **Step 3: config 追加と呼び出し元更新**

RiskConfig に追加（Phase 2 と同じパターン）:

```rust
/// PositionSizer の維持率バッファ。0 で従来挙動 (SLヒット=閾値ちょうど)。
#[serde(default = "default_sizing_margin_buffer")]
pub sizing_margin_buffer: Decimal,

fn default_sizing_margin_buffer() -> Decimal {
    Decimal::new(10, 2) // 0.10
}
```

validate: `sizing_margin_buffer < 0` で bail。`config/default.toml` の `[risk]` に `sizing_margin_buffer = 0.10` を追記。main.rs の `PositionSizer::new` 呼び出しに config 値を渡す。テストコードの呼び出しは `Decimal::ZERO`（従来挙動検証）と `dec!(0.10)`（新テスト）を使い分ける。

- [ ] **Step 4: テスト + commit**

Run: `cargo test -p auto-trader-executor && cargo test -p auto-trader-core`
Expected: PASS

```bash
git add -A
git commit -m "feat(executor): sizing margin buffer — post-SL margin lands above liquidation level"
```

### Task 3.2: NotifyEvent::SystemAlert（汎用アラート）

**Files:**
- Modify: `crates/notify/src/lib.rs`

- [ ] **Step 1: variant を追加**

```rust
#[derive(Debug, Clone, Serialize)]
pub struct SystemAlertEvent {
    /// 例: "margin warn", "margin critical", "balance drift"
    pub title: String,
    pub account_name: String,
    pub exchange: Exchange,
    pub body: String,
}
```

`NotifyEvent` enum（L46-50）に `SystemAlert(SystemAlertEvent)` を追加。`variant_name`（L55-61）に `"system_alert"` arm を追加。Slack フォーマット（L173 付近の match）に arm を追加:

```rust
NotifyEvent::SystemAlert(e) => format!(
    ":rotating_light: *{}* — {} [{}]\n{}",
    e.title,
    e.account_name,
    e.exchange.as_str(),
    e.body
),
```

（既存 arm の書式・絵文字規約に合わせて微調整可。コンパイラが網羅性エラーで他の match 箇所も教えてくれるので全て埋める。）

- [ ] **Step 2: テスト + commit**

既存の notify テスト（L220 付近、OrderFailed の送信テスト）を複製して SystemAlert 版を追加し、mock webhook が受信する本文に title が含まれることを assert。

Run: `cargo test -p auto-trader-notify`
Expected: PASS

```bash
git add -A
git commit -m "feat(notify): generic SystemAlert event for margin/balance alerts"
```

### Task 3.3: live 口座の維持率アラート

**Files:**
- Create: `crates/app/src/margin_alert.rs`
- Modify: `crates/app/src/lib.rs`（mod 追加）、`crates/app/src/main.rs`（liquidation 判定と同じループ）

- [ ] **Step 1: 失敗するテストを書く**

`margin_alert.rs` は `liquidation.rs` の鏡像（live 専用・close せずアラート計画を返す pure 寄り関数）として実装する。まず判定ロジックの unit テスト:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn classify_levels() {
        let y = dec!(0.50); // bitflyer_cfd
        // ratio >= Y×1.3 → None / Y×1.1 <= ratio < Y×1.3 → warn / ratio < Y×1.1 → critical
        assert_eq!(classify_alert_level(dec!(0.70), y), None);
        assert_eq!(classify_alert_level(dec!(0.64), y), Some(AlertLevel::Warn));
        assert_eq!(classify_alert_level(dec!(0.54), y), Some(AlertLevel::Critical));
    }
}
```

- [ ] **Step 2: 実装**

```rust
//! live account の維持率アラート。paper のロスカット (liquidation.rs) と
//! 対になる live 側の監視。**close はしない** — live のロスカット執行は
//! 取引所の責務。bot は接近を運用者に知らせるだけ。
//!
//! 注意: ここで使う残高は DB 管理値であり、取引所実残高とはドリフトしうる
//! (Phase 5 の balance_drift がドリフト自体を監視する)。

use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertLevel {
    Warn,     // ratio < Y × 1.3
    Critical, // ratio < Y × 1.1
}

impl AlertLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

/// 維持率 ratio をアラートレベルに分類する。pure 関数。
/// `Decimal::new(11, 1)` = 1.1, `Decimal::new(13, 1)` = 1.3。
pub fn classify_alert_level(ratio: Decimal, liquidation_level: Decimal) -> Option<AlertLevel> {
    let critical = liquidation_level * Decimal::new(11, 1); // Y × 1.1
    let warn = liquidation_level * Decimal::new(13, 1); // Y × 1.3
    if ratio < critical {
        Some(AlertLevel::Critical)
    } else if ratio < warn {
        Some(AlertLevel::Warn)
    } else {
        None
    }
}
```

続いて検出関数。`liquidation.rs::detect_liquidation_targets`（L41-154）をテンプレートに、次の差分で `detect_margin_alerts` を実装する:

- L73-76 のフィルタを反転: `if dry_run { continue; }`（**live のみ**対象）
- 閾値未満で trade_ids を返す代わりに `classify_alert_level(ratio, threshold)` の Some を `MarginAlert { account_id, account_name: account.name, ratio, threshold, level }` として返す
- 戻り値: `Vec<MarginAlert>`

- [ ] **Step 3: main.rs 配線**

`grep -n "detect_liquidation_targets" crates/app/src/main.rs` で呼び出し箇所を特定し、同じ場所で `detect_margin_alerts` も呼ぶ。レート制限（同一 account×level につき 30 分に 1 回）はループ外に `HashMap<(Uuid, &'static str), std::time::Instant>` を持って判定:

```rust
let alerts = margin_alert::detect_margin_alerts(&mctx, &open_trades, &event).await;
for alert in alerts {
    let key = (alert.account_id, alert.level.as_str());
    let now = std::time::Instant::now();
    let should_send = margin_alert_last
        .get(&key)
        .is_none_or(|last| now.duration_since(*last) > std::time::Duration::from_secs(1800));
    if !should_send {
        continue;
    }
    margin_alert_last.insert(key, now);
    let ev = auto_trader_notify::NotifyEvent::SystemAlert(auto_trader_notify::SystemAlertEvent {
        title: format!("margin {}", alert.level.as_str()),
        account_name: alert.account_name.clone(),
        exchange: event.exchange,
        body: format!(
            "maintenance ratio {} approaching liquidation level {} (live account — exchange will liquidate below threshold)",
            alert.ratio.round_dp(4),
            alert.threshold
        ),
    });
    let notifier = notifier.clone();
    tokio::spawn(async move {
        if let Err(e) = notifier.send(ev).await {
            tracing::warn!("margin alert notify failed: {e}");
        }
    });
}
```

- [ ] **Step 4: 統合テスト + 全体テスト + PR**

`crates/integration-tests/tests/phase3_liquidation_safety.rs` のパターンを流用し、live 口座（`[live].enabled` は不要 — アラートは発注ではない）+ 含み損 trade を seed して、warn/critical 分類が期待通りかを `detect_margin_alerts` 単位で検証する統合テストを追加。

Run: `./scripts/test-all.sh` → Expected: `ALL GREEN`

```bash
git add -A
git commit -m "feat(app): live margin-ratio alerts (warn/critical, rate-limited, no auto-close)"
```

specs 更新（`specs/design.md` に「live 維持率はアラートのみ、執行は取引所」）+ PR。

---

# Phase 4: 取引所側ストップ注文（SL の取引所執行）

**ブランチ:** `feat/exchange-stop-orders`
**目的:** ブロッカー #4。live エントリー成功後、取引所側に SL のストップ（逆指値）注文を置く。アプリが死んでいても SL が執行される。アプリ側 tick 監視は**バックアップとして残す**。paper (dry_run) はストップ注文を置かない（従来どおりアプリ側シミュレーションが正）。

**設計上の不変条件:**
- ストップ注文の発注失敗は **open を失敗させない**（ポジションは既に立っている）。Slack critical アラート + アプリ側 SL 監視でカバー。
- アプリ側から close する時は**必ず先にストップ注文の状態を確認**する。Executed ならその約定でクローズ記録（二重クローズ＝意図しない反対ポジションを絶対に作らない）。Active なら cancel してから成行 close。cancel 失敗（執行との race）なら再度 status 確認、それでも不明なら close を中断してエラー（stale-lock 自己回復に委ねる）。

### Task 4.0: 公式 API 仕様の確認（実装前必須）

- [ ] **Step 1: bitFlyer 特殊注文 API を確認**

WebFetch: `https://lightning.bitflyer.com/docs?lang=ja`（Lightning API リファレンス）
確認事項: `POST /v1/me/sendparentorder`（order_method="SIMPLE", condition_type="STOP" の parameters 形式、trigger_price フィールド名）、`POST /v1/me/cancelparentorder`、`GET /v1/me/getparentorder`（parent_order_acceptance_id → parent_order_id 解決）、`GET /v1/me/getchildorders?parent_order_id=`（発火後の child order 取得）。

- [ ] **Step 2: GMO Coin FX 注文 API を確認**

WebFetch: `https://api.coin.z.com/fxdocs/`（外国為替FX API リファレンス）
確認事項: `POST /private/v1/closeOrder`（executionType="STOP" 時の price/settlePosition 形式）、`POST /private/v1/cancelOrder`、`GET /private/v1/orders?orderId=`（status 値の一覧: 例 EXECUTED / CANCELED / EXPIRED / ORDERED / WAITING）、`GET /private/v1/executions?orderId=`。

- [ ] **Step 3: 確認結果をこの plan ファイルの末尾に「API 検証メモ」として追記 commit**

以降のタスクのコードでフィールド名が公式と食い違う場合、**公式を正として**コードを直すこと。

### Task 4.1: ExchangeApi trait 拡張 + Trade.stop_order_id

**Files:**
- Modify: `crates/market/src/exchange_api.rs`
- Modify: `crates/core/src/types.rs`（Trade struct）
- Create: `migrations/20260709000001_add_stop_order_id.sql`
- Modify: `crates/db/src/trades.rs`（insert/select の列追加）

- [ ] **Step 1: migration**

```sql
-- 取引所側 SL ストップ注文の ID (bitFlyer: parent_order_acceptance_id,
-- GMO: closeOrder の orderId)。dry_run (paper) は常に NULL。
ALTER TABLE trades ADD COLUMN stop_order_id TEXT;
```

- [ ] **Step 2: trait 拡張**

`exchange_api.rs` に追加:

```rust
/// 取引所側ストップ (逆指値) 注文の状態。
#[derive(Debug, Clone, PartialEq)]
pub enum StopOrderStatus {
    /// まだ発火していない
    Active,
    /// 発火して約定済み。price は加重平均約定価格。
    Executed { price: Decimal, commission: Decimal },
    /// キャンセル / 失効 / 拒否 (もはや存在しない)
    Gone,
}
```

trait にデフォルト実装付きメソッドを 3 つ追加（デフォルトは bail — dry_run / 未対応取引所からは呼ばれない設計であり、呼ばれたら即座に露見させる）:

```rust
/// SL ストップ注文を置く。戻り値は取引所発行の注文 ID。
/// `position_id` は GMO のように close 対象 position の指定が必要な
/// 取引所で Some、bitFlyer (netting) では None。
async fn place_stop_order(
    &self,
    _product_code: &str,
    _close_side: Side,
    _size: Decimal,
    _trigger_price: Decimal,
    _position_id: Option<&str>,
) -> anyhow::Result<String> {
    anyhow::bail!("place_stop_order not supported on this exchange")
}

async fn cancel_stop_order(&self, _product_code: &str, _stop_order_id: &str) -> anyhow::Result<()> {
    anyhow::bail!("cancel_stop_order not supported on this exchange")
}

async fn stop_order_status(
    &self,
    _product_code: &str,
    _stop_order_id: &str,
) -> anyhow::Result<StopOrderStatus> {
    anyhow::bail!("stop_order_status not supported on this exchange")
}
```

- [ ] **Step 3: Trade struct に `stop_order_id: Option<String>` を追加**

`crates/core/src/types.rs` の Trade に `pub stop_order_id: Option<String>,` を追加し、`cargo check --workspace` のエラーを潰していく。既知の変更箇所: `crates/db/src/trades.rs`（insert_trade の列リスト・bind、SELECT 列と row マッピング）、`crates/executor/src/trader.rs`（Trade リテラル 2 箇所 — execute では後で値を入れるためいったん `None`、close では `trade.stop_order_id.clone()`）、`crates/backtest/src/runner.rs`（`None`）、integration-tests の helper / mock で Trade を組む箇所（`None`）。

- [ ] **Step 4: テスト + commit**

Run: `cargo test --workspace --lib` → Expected: PASS

```bash
git add -A
git commit -m "feat(core,db,market): stop_order_id column + ExchangeApi stop-order methods"
```

### Task 4.2: bitFlyer 実装（親注文 SIMPLE/STOP）

**Files:**
- Modify: `crates/market/src/bitflyer_private.rs`

- [ ] **Step 1: 失敗するテストを書く**

`crates/market/tests/bitflyer_private_test.rs` の既存テスト（mock サーバ方式）を踏襲し、`place_stop_order` が `/v1/me/sendparentorder` に以下の JSON を POST することを検証:

```json
{
  "order_method": "SIMPLE",
  "parameters": [{
    "product_code": "FX_BTC_JPY",
    "condition_type": "STOP",
    "side": "SELL",
    "size": 0.004,
    "trigger_price": 12250000
  }]
}
```

レスポンス `{"parent_order_acceptance_id": "JRF20260707-000000-000000"}` から ID が返ることを assert。

- [ ] **Step 2: 実装**

`bitflyer_private.rs` に request/response 型を追加（既存 `SendChildOrderRequest` の Serialize 規約に合わせる）:

```rust
#[derive(Debug, Serialize)]
struct ParentOrderParameter {
    product_code: String,
    condition_type: &'static str, // "STOP"
    side: Side,
    size: Decimal,
    trigger_price: Decimal,
}

#[derive(Debug, Serialize)]
struct SendParentOrderRequest {
    order_method: &'static str, // "SIMPLE"
    parameters: Vec<ParentOrderParameter>,
}

#[derive(Debug, Deserialize)]
struct SendParentOrderResponse {
    parent_order_acceptance_id: String,
}
```

`impl ExchangeApi for BitflyerPrivateApi`（既存 impl ブロック）に override を追加。HTTP 呼び出し・HMAC 署名は既存の send_child_order が使う内部ヘルパをそのまま使う:

- `place_stop_order`: sendparentorder に POST → acceptance_id を返す。`position_id` は無視（bitFlyer は netting）。
- `cancel_stop_order`: `POST /v1/me/cancelparentorder` body `{"product_code": ..., "parent_order_acceptance_id": ...}`。
- `stop_order_status`:
  1. `GET /v1/me/getparentorder?parent_order_acceptance_id=...` で parent_order の詳細（`parent_order_id` と state）を取得。404/空 → `Gone`
  2. state が ACTIVE → `Active`
  3. state が COMPLETED → `GET /v1/me/getchildorders?product_code=...&parent_order_id=...` で発火した child を取得し、その `child_order_acceptance_id` で既存 `get_executions` を呼び、既存の `aggregate_executions` と同形の加重平均で `Executed { price, commission }` を作る
  4. CANCELED / EXPIRED / REJECTED → `Gone`

- [ ] **Step 3: status/cancel の mock テストを追加**（Executed 経路: getparentorder → getchildorders → getexecutions の 3 hop を mock）

Run: `cargo test -p auto-trader-market bitflyer` → Expected: PASS

- [ ] **Step 4: commit**

```bash
git add -A
git commit -m "feat(market): bitFlyer parent-order STOP — place/cancel/status"
```

### Task 4.3: GMO FX 実装（closeOrder STOP）

**Files:**
- Modify: `crates/market/src/gmo_fx_private.rs`

- [ ] **Step 1-4: Task 4.2 と同じ進め方**

`impl ExchangeApi for GmoFxPrivateApi`（gmo_fx_private.rs:390）に override を追加。既存の注文送信ヘルパ（署名・エラーマッピング `status=5` 等）を再利用する。

- `place_stop_order`: `POST /private/v1/closeOrder`

```json
{
  "symbol": "USD_JPY",
  "side": "SELL",
  "executionType": "STOP",
  "price": "156.500",
  "size": "1592",
  "settlePosition": [{"positionId": 123456, "size": "1592"}]
}
```

`position_id` が None なら bail（GMO は必須。`requires_close_position_id` と同じ理由）。price は GMO の桁規約（Task 4.0 で確認した tick）に従い文字列化。戻り値は response の orderId（文字列化して返す）。

- `cancel_stop_order`: `POST /private/v1/cancelOrder` `{"orderId": ...}`。「既に約定済み」を示すエラーコードは Err のまま返してよい（呼び出し側が status 再確認する契約）。
- `stop_order_status`: `GET /private/v1/orders?orderId=...` → status が EXECUTED → `GET /private/v1/executions?orderId=...` で約定リストを取得し加重平均で `Executed`。ORDERED/WAITING → `Active`。CANCELED/EXPIRED → `Gone`。

テストは `crates/integration-tests/src/mocks/gmo_fx_server.rs` にエンドポイントを追加して phase3 系 or market の単体 mock テストで検証（既存の GMO テストがどちらの方式か確認して合わせる）。

```bash
git add -A
git commit -m "feat(market): GMO FX stop close-order — place/cancel/status"
```

### Task 4.4: trigger price の tick 丸め

**Files:**
- Modify: `crates/executor/src/position_sizer.rs`（price_units を持たせる）
- Modify: PositionSizer::new 呼び出し元（main.rs — config の `pair_config.price_unit` を渡す）

- [ ] **Step 1: 失敗するテストを書く**

```rust
/// Long の SL trigger は切り上げ (早く発火する側 = 損失が小さい側)、
/// Short は切り捨て。
#[test]
fn trigger_price_rounds_to_safe_side() {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let mut units = HashMap::new();
    units.insert(Pair::new("USD_JPY"), dec!(0.001));
    let sizer = PositionSizer::new(min_sizes, dec!(0.10)).with_price_units(units);
    // Long SL 156.78912 → 156.790 (ceil to 0.001)
    assert_eq!(
        sizer.round_trigger_price(&Pair::new("USD_JPY"), dec!(156.78912), Direction::Long),
        dec!(156.790)
    );
    // Short SL 156.78912 → 156.789 (floor)
    assert_eq!(
        sizer.round_trigger_price(&Pair::new("USD_JPY"), dec!(156.78912), Direction::Short),
        dec!(156.789)
    );
}
```

- [ ] **Step 2: 実装**

```rust
// PositionSizer に追加
price_units: HashMap<Pair, Decimal>, // default empty

pub fn with_price_units(mut self, price_units: HashMap<Pair, Decimal>) -> Self {
    self.price_units = price_units;
    self
}

/// SL trigger price を取引所 tick (pair_config.price_unit) に丸める。
/// Long の SL は下側にあるので「早く発火する側」= 切り上げ、
/// Short の SL は上側なので切り捨て。丸め方向を誤ると SL 価格を
/// わずかに超えた損失で発火することになる。
pub fn round_trigger_price(&self, pair: &Pair, price: Decimal, direction: Direction) -> Decimal {
    let unit = self.price_units.get(pair).copied().unwrap_or(Decimal::ZERO);
    if unit <= Decimal::ZERO {
        return price;
    }
    let steps = price / unit;
    let rounded = match direction {
        Direction::Long => steps.ceil(),
        Direction::Short => steps.floor(),
    };
    rounded * unit
}
```

（`Direction` は `auto_trader_core::types::Direction` を use する。）main.rs で `pair_config` から `HashMap<Pair, Decimal>`（price_unit）を組んで `with_price_units` を通す。

- [ ] **Step 3: テスト + commit**

```bash
git add -A
git commit -m "feat(executor): trigger-price tick rounding toward the safe side"
```

### Task 4.5: Trader への配線（open で place / close で status→cancel）

**Files:**
- Modify: `crates/executor/src/trader.rs`

- [ ] **Step 1: open 側 — `execute()` の exchange_position_id 解決（L851-881）の直後に追加**

```rust
// 取引所側 SL ストップ注文 (live のみ)。失敗しても open は成立させる —
// ポジションは既に立っており、アプリ側 SL 監視がバックアップとして働く。
// 失敗は critical 通知して運用者に知らせる。
let stop_order_id = if self.dry_run {
    None
} else {
    let close_side = match signal.direction {
        Direction::Long => Side::Sell,
        Direction::Short => Side::Buy,
    };
    let trigger =
        self.position_sizer
            .round_trigger_price(&signal.pair, stop_loss, signal.direction);
    match self
        .api
        .place_stop_order(
            &signal.pair.0,
            close_side,
            actual_qty,
            trigger,
            exchange_position_id.as_deref(),
        )
        .await
    {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::error!(
                "place_stop_order failed for {} — position is UNPROTECTED at the exchange \
                 (app-side SL monitoring is the only stop): {e}",
                signal.pair
            );
            let notifier = self.notifier.clone();
            let ev = NotifyEvent::OrderFailed(OrderFailedEvent {
                account_name: self.account_name.clone(),
                exchange: self.exchange,
                strategy_name: signal.strategy_name.clone(),
                pair: signal.pair.clone(),
                reason: format!(
                    "exchange-side stop order FAILED (position unprotected if app dies): {e}"
                ),
            });
            tokio::spawn(async move {
                if let Err(e) = notifier.send(ev).await {
                    tracing::warn!("stop-order-failure alert send failed: {e}");
                }
            });
            None
        }
    }
};
```

Trade リテラルの `stop_order_id: None,` を `stop_order_id,` に差し替える。

- [ ] **Step 2: close 側 — `fill_close()` の live 分岐（`else` ブロック, L403-415）の先頭に追加**

```rust
// 取引所側ストップ注文が置かれている場合、成行 close の前に必ず状態確認。
// Executed → その約定でクローズ記録 (成行を重ねると反対ポジションが立つ)。
// Active → cancel してから成行。cancel 失敗は執行との race の可能性が
// あるので再確認し、それでも不明なら close を中断 (stale-lock 自己回復に委ねる)。
if let Some(stop_id) = &trade.stop_order_id {
    match self.api.stop_order_status(&trade.pair.0, stop_id).await {
        Ok(StopOrderStatus::Executed { price, commission }) => {
            tracing::info!(
                "close: stop order {stop_id} already executed at {price}; recording without new order"
            );
            return Ok((price, commission));
        }
        Ok(StopOrderStatus::Active) => {
            if let Err(cancel_err) = self.api.cancel_stop_order(&trade.pair.0, stop_id).await {
                if let Ok(StopOrderStatus::Executed { price, commission }) =
                    self.api.stop_order_status(&trade.pair.0, stop_id).await
                {
                    return Ok((price, commission));
                }
                anyhow::bail!(
                    "cancel_stop_order failed for {stop_id} and status unclear: {cancel_err}; \
                     aborting close to avoid double-close"
                );
            }
        }
        Ok(StopOrderStatus::Gone) => {
            tracing::warn!("close: stop order {stop_id} already gone (canceled/expired)");
        }
        Err(e) => {
            anyhow::bail!("stop_order_status failed for {stop_id}: {e}; aborting close");
        }
    }
}
```

`use auto_trader_market::exchange_api::StopOrderStatus;` を追加。`fill_close_size`（stale 復旧の部分クローズ, L674）にも同じガードを先頭に入れる。

- [ ] **Step 3: 統合テスト**

`crates/integration-tests/src/mocks/exchange_api.rs`（MockExchangeApi）に stop-order 3 メソッドの mock 実装と呼び出し記録を追加し、`phase3_execution_flow.rs` パターンで:

1. live trade を開く → `place_stop_order` が trigger=丸め済み SL 価格で呼ばれ、`trades.stop_order_id` が保存されること
2. アプリ経由 close → `stop_order_status` → `cancel_stop_order` → 成行 close の順で呼ばれること
3. mock が `Executed{price}` を返す時 → 新規注文が**発行されず**、exit_price=stop 約定価格で closed になること
4. `place_stop_order` が Err の時 → open は成功し stop_order_id NULL、OrderFailed 通知が飛ぶこと

- [ ] **Step 4: 全体テスト + commit**

Run: `./scripts/test-all.sh` → Expected: `ALL GREEN`

```bash
git add -A
git commit -m "feat(executor): place exchange-side SL on open, status-check/cancel on close"
```

### Task 4.6: ストップ発火の検知（定期ジョブ + startup reconcile）

**Files:**
- Modify: `crates/app/src/main.rs`（ジョブ群 — swap/SFD ジョブ L1713 以降と同じパターン）
- Modify: `crates/app/src/startup_reconcile.rs`

- [ ] **Step 1: 定期検知ジョブ**

60 秒間隔の tokio タスクを追加（swap ジョブの structure を踏襲）。処理:

1. live 口座の open trade のうち `stop_order_id IS NOT NULL` を列挙（`liquidation.rs` が使っている `OpenTradeWithAccount` クエリを流用し、live + stop_order_id でフィルタ）
2. 各 trade について `api.stop_order_status()` を呼ぶ
3. `Executed { .. }` なら `closer::close_trade(ctx, &trade, name, account_type, /*dry_run=*/false, ExitReason::SlHit, current_price)` を呼ぶ。close_trade → close_position → fill_close は Step 4.5 のガードにより「Executed を検出してその約定価格を返す」ので、**二重発注にはならない**（この経路が成立することが 4.5 Step 3 のテスト 3 で保証されている）
4. `Active` / `Gone` は何もしない（Gone + position 残存は margin alert / 手動対応領域。warn ログのみ）

- [ ] **Step 2: startup_reconcile の拡張**

`startup_reconcile.rs` を読み、「DB=open だが exchange に position が無い」ケースの分岐（現状: force close, exit_reason=reconciled）を特定する。そこに追加:

- trade に `stop_order_id` があれば `stop_order_status` を確認し、`Executed { price, commission }` なら exit_price=price / exit_reason=`SlHit` / fees に commission 加算で close する（従来の「best-effort 価格で reconciled」より正確な記録になる）
- Executed でなければ従来どおり reconciled 扱い

また「DB=open かつ exchange に position がある」正常ケースでは、`stop_order_id` があるのに status が `Gone` の場合に SystemAlert（"stop order lost — position unprotected"）を出す。

- [ ] **Step 3: 統合テスト**

startup_reconcile の既存テスト（`phase3_reconcile.rs`）に「position 無し + stop Executed → SlHit close で正確な価格」を追加。

- [ ] **Step 4: 全体テスト + specs 更新 + commit + PR**

Run: `./scripts/test-all.sh` → Expected: `ALL GREEN`

specs: `specs/design.md` の SL/TP 監視の節（L139-144）に「live は取引所側ストップ注文が一次防衛、アプリ tick 監視は二次」と追記。

```bash
git add -A
git commit -m "feat(app): stop-fill detection job + reconcile stop-executed trades as sl_hit"
```

---

# Phase 5: live 残高照合（ドリフト検知）

**ブランチ:** `feat/balance-drift-check`
**依存:** Phase 3（NotifyEvent::SystemAlert）
**目的:** ブロッカー #5。live 口座の bot 管理 equity と取引所報告 equity を起動時 + 毎時比較し、乖離が閾値を超えたら Slack 警告。**自動補正はしない**（台帳不変条件 `current_balance = initial + Σpnl − Σfees` を守る。補正は運用者判断）。

### Task 5.1: 照合ロジック

**Files:**
- Create: `crates/app/src/balance_drift.rs`
- Modify: `crates/app/src/lib.rs`

- [ ] **Step 1: get_collateral の GMO 実装を確認**

`crates/market/src/gmo_fx_private.rs` の `get_collateral` 実装を読む。`/private/v1/account/assets` にマップされ equity 相当（Collateral.collateral + open_position_pnl で純資産になる形）が返るか確認。bail スタブなら本タスク内で実装する（レスポンスの equity / availableAmount フィールドは Task 4.0 と同じ doc で確認）。

- [ ] **Step 2: 失敗するテストを書く（pure 部分）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn drift_threshold_is_1pct_or_500yen_whichever_larger() {
        // exchange equity 30,000 → threshold = max(300, 500) = 500
        assert!(!is_drift(dec!(30000), dec!(30400)));
        assert!(is_drift(dec!(30000), dec!(30501)));
        // exchange equity 1,000,000 → threshold = max(10000, 500) = 10000
        assert!(!is_drift(dec!(1000000), dec!(1009999)));
        assert!(is_drift(dec!(1000000), dec!(1010001)));
    }
}
```

- [ ] **Step 3: 実装**

```rust
//! live 口座の残高ドリフト検知。bot は current_balance を DB 台帳で管理
//! するが、live では swap/SFD/手数料を取引所が直接徴収するため、実残高
//! とは徐々に乖離する。乖離を検知して運用者に知らせる (自動補正はしない
//! — 台帳の不変条件 current_balance = initial + Σpnl − Σfees を壊さない)。

use rust_decimal::Decimal;

/// ドリフト判定。閾値 = max(取引所 equity の 1%, 500 円)。
pub fn is_drift(exchange_equity: Decimal, bot_equity: Decimal) -> bool {
    let threshold = (exchange_equity * Decimal::new(1, 2)).max(Decimal::from(500));
    (exchange_equity - bot_equity).abs() > threshold
}
```

続いて照合関数 `check_live_accounts`:

1. `trading_accounts::list_all` から `account_type == "live"` を抽出（`live_forces_dry_run` 時は skip — dry_run では取引所残高は動かない）
2. 各口座: `api.get_collateral()` → `exchange_equity = collateral + open_position_pnl`
3. bot 側: `bot_equity = current_balance + Σrequired_margin + Σunrealized_pnl`。open trades を読み、`liquidation.rs` L96-134 と同じ方法（PriceStore の close-side bid/ask、`core::margin::OpenPosition`）で組む。価格が無い trade がある口座は skip（warn ログ）
4. `is_drift` なら `SystemAlertEvent { title: "balance drift", body: "exchange equity={} bot equity={} diff={}" }` を返す

戻り値 `Vec<SystemAlertEvent>` として、送信は呼び出し側（main.rs）で行う。

### Task 5.2: 起動時 + 毎時ジョブへの配線

**Files:**
- Modify: `crates/app/src/main.rs`

- [ ] **Step 1: 配線**

- 起動時: startup_reconcile の直後に `check_live_accounts` を 1 回実行し、アラートがあれば送信（起動をブロックしない — warn のみ）
- 毎時: swap/SFD ジョブ（main.rs:1713 以降）と同じパターンで 3600 秒間隔タスクを追加

- [ ] **Step 2: 統合テスト**

MockExchangeApi の `get_collateral` に任意値を返させ、DB seed との乖離ケース/一致ケースで SystemAlert の有無を検証（`phase3_jobs.rs` のパターン）。

- [ ] **Step 3: 全体テスト + specs 更新 + commit + PR**

Run: `./scripts/test-all.sh` → Expected: `ALL GREEN`

```bash
git add -A
git commit -m "feat(app): hourly live balance drift detection vs exchange equity"
```

---

# Phase 6: regime 窓長修正 + backtest 近代化

**ブランチ:** `feat/backtest-modernize`
**目的:** ブロッカー #6（エッジ未実証）の解消手段を整える。(a) H1 イベントに M5 指標が無印で載る不整合を修正（週次進化の regime 集計が誤ラベルになる）。(b) backtest crate を bitFlyer/GMO の現行戦略で使える状態にする（exchange パラメータ化・PositionSizer サイジング・戦略 exit 再生・PnL バグ修正）。

### Task 6.1: H1 イベントの regime 指標を H1 ネイティブに

**Files:**
- Modify: `crates/market/src/bitflyer.rs`（L462-502）

- [ ] **Step 1: 現状確認**

`emit_candle_event` のシグネチャ（同ファイル内）を確認する。現状 H1 candle には `latest_indicators`（M5 で計算した値）が無印 + prefix 付きの両方で載る（L479-489）。また `crates/strategy/` 内で `event.indicators` を参照している戦略が無いことを `grep -rn "indicators" crates/strategy/src` で確認する（戦略は自前で candle 履歴から計算しているはず。もし参照があれば影響を評価してから進める）。

- [ ] **Step 2: 修正**

H1 用の履歴 map（`closes_map_h1` / `highs_map_h1` / `lows_map_h1`、M5 用と同じ型）をループ外に用意し、H1 candle 完成時に `emit_candle_event` を H1 履歴で呼んで **H1 ネイティブ指標**を無印で載せる。M5 由来値は prefix 付き（`m5_` 等）のみ残す:

```rust
let (mut h1_event, _h1_indicators) = emit_candle_event(
    h1_candle,
    closes_map_h1,
    highs_map_h1,
    lows_map_h1,
    true,
);
// M5 指標は prefix 付きでのみ添付 (無印は H1 ネイティブ値)。
// これにより H1 戦略の entry_indicators / regime 分類が H1 の
// ADX / ATR percentile で行われる (従来は M5 値が無印で載っており、
// weekly_batch の regime 別 Wilson 集計が誤った時間足でラベルされていた)。
if let Some(m5) = latest_indicators.get(product_code) {
    for (key, value) in m5 {
        h1_event
            .indicators
            .insert(format!("{primary_tf_prefix}_{key}"), *value);
    }
}
if price_tx.send(h1_event).await.is_err() { ... }
```

- [ ] **Step 3: テスト**

bitflyer.rs / candle_builder の既存テスト方式を確認し、「H1 イベントの `adx_14` が H1 履歴由来である（M5 値と異なる）」ことを検証するテストを追加。tick 列を流して M5 と H1 で異なる履歴を作る fixture は `phase3_squeeze_momentum.rs` 等の投入パターンが参考になる。

- [ ] **Step 4: commit**

```bash
git add -A
git commit -m "fix(market): H1 events carry H1-native indicators; M5 values prefixed only"
```

注: この変更で weekly_batch の regime 集計の意味が変わる（過去データと連続しない）。commit メッセージと specs に「2026-07-XX 以前の H1 regime ラベルは M5 由来」と明記する。

### Task 6.2: backtest runner の近代化

**Files:**
- Modify: `crates/backtest/src/runner.rs`
- Modify: `crates/backtest/src/lib.rs`（公開シグネチャ変更の反映）

- [ ] **Step 1: 呼び出し元の確認**

Run: `grep -rn "BacktestRunner" crates/ --include="*.rs" | grep -v target | grep -v backtest/src`
呼び出し元（bin / テスト）のシグネチャ変更影響を把握する。

- [ ] **Step 2: SimTrader を書き換える**

`runner.rs` の SimTrader / run を以下の仕様に書き換える（現行コードは L19-22 の NOTE どおり FX 専用のため大部分を置換してよい）:

1. `BacktestRunner::run` のシグネチャを変更:

```rust
pub async fn run(
    &self,
    strategy: &mut dyn Strategy,
    exchange: Exchange,               // 追加: "bitflyer_cfd" / "gmo_fx"
    pair: &Pair,
    timeframe: &str,
    initial_balance: Decimal,
    leverage: Decimal,
    sizer: &PositionSizer,            // 追加: 本番と同じサイザー
    liquidation_margin_level: Decimal, // 追加: [exchange_margin] の Y
) -> anyhow::Result<BacktestReport>
```

candle 取得の `"oanda"` ハードコード（L141）を `exchange.as_str()` に置換。

2. `SimTrader::open` で quantity を本番と同一のロジックで決める:

```rust
let quantity = match sizer.calculate_quantity(
    &signal.pair,
    self.balance,
    entry_price,
    self.leverage,
    signal.allocation_pct,
    signal.stop_loss_pct,
    liquidation_margin_level,
) {
    Some(q) => q,
    None => return None, // 残高不足 → execution_failures にカウント
};
```

（open の戻り値を `Option<Trade>` に変え、None は runner 側で `execution_failures += 1`。）

3. **PnL バグ修正**: `close` の `let pnl_amount = price_diff * self.leverage;`（L102）を本番 trader.rs:1108 と同一の式に置換:

```rust
let pnl_amount = (price_diff * trade.quantity).round_dp_with_strategy(0, RoundingStrategy::ToZero);
```

4. **戦略 exit の再生**: candle ループ内、SL/TP チェックの後に `on_open_positions` を呼ぶ:

```rust
let open_positions: Vec<Position> = trader
    .open_positions()
    .into_iter()
    .map(|trade| Position { trade })
    .collect();
let exit_signals = strategy.on_open_positions(&event, &open_positions).await;
for exit in exit_signals {
    let closed = trader.close(
        exit.trade_id,
        exit.reason.to_exit_reason(),
        exit.close_price,
        candle.timestamp,
    )?;
    trades.push(closed);
}
```

（`on_open_positions` の正確なシグネチャは `crates/core/src/strategy.rs:132` を確認して合わせる。）

5. スプレッド近似: 約定価格に `spread_pct`（run の追加引数、デフォルト検証は 0.01% 程度）を entry は不利側・exit も不利側に適用する。candle には bid/ask が無いため定率近似とし、**doc コメントに「板深さ・実スプレッド変動は未モデル」と明記**する。

6. 指標の供給: 現行の sma/rsi だけでなく、対象戦略が `event.indicators` に依存しないこと（Task 6.1 Step 1 で確認済み）を前提に、指標 map は現状維持でよい。

- [ ] **Step 3: テスト**

`crates/backtest` にテストを追加:

- 合成 candle 列（単調上昇 → 急落）を DB に seed（`#[sqlx::test]` + `upsert_candle`）し、donchian_trend_v1 を流して「entry が発生する」「SL close の pnl が quantity ベースで計算される（leverage 掛け算バグの regression guard）」を assert
- `pnl_amount = (exit - entry) × quantity` の値を具体的な数値で検証するケースを必ず 1 つ入れる

- [ ] **Step 4: 全体テスト + specs 更新 + commit + PR**

Run: `./scripts/test-all.sh` → Expected: `ALL GREEN`

specs: `specs/design.md` backtest の節（L178-182）を現状に合わせて更新（exchange 対応・サイジング一致・スプレッド近似の限界）。

```bash
git add -A
git commit -m "feat(backtest): exchange-aware runner with production sizing, strategy exits, qty-based pnl"
```

---

# 運用移行手順（コード外 — 全 Phase 完了後）

これはコードではなく運用チェックリスト。live 切り替えは以下の段階を**順に**踏む。各段階で問題が出たら前の段階に戻る。

### Stage 0: 検証データの蓄積（並行して常時）

- [ ] Phase 6 の backtest で全 4 戦略 × FX_BTC_JPY / USD_JPY を蓄積済み candle で回し、明らかに負ける戦略を足切りする
- [ ] paper 運用を継続し、**戦略ごとに 100 トレード以上**を蓄積（`docs/strategy-performance-review-2026-04-19.md` の指摘水準）。週次で Wilson 下限勝率と R:R を確認
- [ ] paper 成績がプラスの戦略が 1 つも無いうちは Stage 1 に進まない

### Stage 1: live 経路の dry-run 検証（資金リスクなし）

- [ ] bitFlyer の API key/secret を発行（**取引権限のみ、出金権限は付けない**）し、1Password + direnv で環境変数供給
- [ ] live 口座行を trading_accounts に作成（最小資金、例: 30,000 円、leverage 2、strategy は paper 成績最上位の 1 つ）
- [ ] `[live].enabled = true` + `LIVE_DRY_RUN=1` で数日運用し、live 口座が dry_run 経路で正常動作すること・Slack 通知・Kill Switch・margin alert が機能することを確認

### Stage 2: 最小 live 運用（bitFlyer のみ）

- [ ] `LIVE_DRY_RUN` を外し `[live].dry_run = false` に設定。**live 口座は 1 つ・1 戦略・FX_BTC_JPY のみ**
- [ ] 初回発注で確認: 取引所側ストップ注文が bitFlyer の注文一覧に見えること、Slack の OrderFilled が実約定価格を示すこと
- [ ] 毎日確認（最初の 2 週間）: balance drift アラートの有無、`trades` の fees が bitFlyer の約定履歴と一致すること、reconcile ログ
- [ ] プロセス再起動テストを 1 回実施: open position がある状態で再起動し、startup_reconcile が正しく復元すること

### Stage 3: 拡大

- [ ] 2-4 週間問題なければ GMO FX 口座を追加（同じく最小資金から）
- [ ] 増資・戦略追加は 1 変更ずつ。同時に 2 つ以上変えない

### 運用 Runbook（specs/design.md に転記すること）

- Kill Switch 手動解除: `UPDATE trading_accounts SET halted_until = NULL, halt_reason = NULL WHERE name = '<account>';`
- 緊急停止: `[live].enabled = false` にして再起動（新規停止・close は継続）。全クローズが必要なら各ポジションを取引所の管理画面から手動決済し、次回起動の startup_reconcile に DB を追随させる
- balance drift アラートが出たら: 取引所の入出金/スワップ履歴を確認し、必要なら `initial_balance` を調整する補正 SQL を記録を残して実行

---

## Self-Review 結果（計画作成時に実施済み）

- ブロッカー #1→Phase 1、#2→Phase 2、#3→Phase 3、#4→Phase 4、#5→Phase 5、#6→Phase 6 + Stage 0 で全件カバー
- 意図的スコープ外: cross-currency 換算（EUR_USD 除外で代替）、live ロスカットの bot 執行（取引所執行 + アラートで代替）、板深さスリッページ（bid/ask 約定 + backtest 定率近似で代替）、残高の自動補正（アラートのみ）
- 型整合: `PositionSizer::new(min_order_sizes, margin_buffer)` は Phase 3 で変更、Phase 4 は builder（`with_price_units`）で拡張のため衝突しない。`StopOrderStatus` / `stop_order_id` / `SystemAlertEvent` の名前は全 Phase で統一
- 既知の不確実性: bitFlyer 親注文 / GMO closeOrder の正確なフィールド名（Task 4.0 で検証必須）、integration-tests helper の正確な API（各テストタスクで既存ファイルのパターン踏襲を指示済み）
