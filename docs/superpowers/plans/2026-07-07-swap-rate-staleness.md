# GMO Swap Rate Staleness Alert Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** GMO スワップレート表（config 手入力の代表値）が古くなったこと・未設定であることを検知して運用者に知らせる。paper=live 近似の質を運用で維持できるようにする。

**Architecture:** スワップポイントは GMO が日次公表する配布値で、価格から導出できず API も存在しない（2026-07-07 に公式 fxdocs で確認済み）。よって config 代表値が構造的上限であり、本計画は「レート表に更新日を持たせ、既存の日次スワップジョブが staleness / 未設定を SystemAlert で知らせる」最小の鮮度管理を足す。自動取得・スクレイピングはしない（YAGNI、2026-05-19 設計の判断を維持）。

**Tech Stack:** Rust / chrono NaiveDate / 既存の SystemAlert (notify crate) / 既存 overnight/swap 日次ジョブ (main.rs ~L1996)

---

## 実装者への必須ルール

1. ブランチ `feat/swap-rate-staleness`（作成済み・checkout 済み）上で作業。**ブランチの作成・切替・reset は一切禁止**。push / PR もしない。
2. Conventional Commits。金額・レートは `rust_decimal::Decimal`。
3. TDD: 失敗するテストを先に書き、落ちることを確認してから実装。
4. Task ごとに commit（2 コミット）。**最終 commit 前に `./scripts/test-all.sh` を実行し `ALL GREEN` を確認**。docker 不可なら `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --lib --bins --tests` で代替し、その旨報告。
5. 実コードが正。計画とファイル実体が食い違ったら実体に合わせる。想定外に影響範囲が広がったら BLOCKED で報告（推測で進めない）。

## 検証済みの既存事実

- `crates/core/src/config.rs:66`: `pub struct GmoFxSwapConfig { pub rates: HashMap<String, SwapRateEntry> }`。`AppConfig.gmo_fx: GmoFxConfig`（L40, `#[serde(default)]`）、`GmoFxConfig { swap: GmoFxSwapConfig }`（L47）。rates はセクション欠落時に空（テスト L748）。
- `config/default.toml` に **`[gmo_fx.swap]` セクションは現状存在しない**（= 本番 config で rates を入れない限り GMO paper のスワップは計上されない）。
- `crates/core/src/swap.rs`: `compute_daily_swap(rate, direction, quantity)` pure 関数。
- `crates/app/src/main.rs:~1996-2010`: overnight/swap 日次ジョブ。`let swap_config = config.gmo_fx.swap.clone();` で clone 済み、60 秒 tick で `today != last_date` のとき（= UTC 日付変更ごとに 1 回）fee を適用する。**notifier はこの task の scope に入っていない**（要配線）。main.rs 内の notifier 変数名は `grep -n "Arc::new(Notifier" crates/app/src/main.rs` と周辺の `.clone()` パターン（例: `crypto_monitor_notifier`）で確認すること。
- `crates/notify/src/lib.rs`: `NotifyEvent::SystemAlert(SystemAlertEvent { title, account_name, exchange, body })`（Phase 3 で追加済み）。
- `Exchange::GmoFx` が `auto_trader_core::types::Exchange` に存在する。
- workspace の chrono は serde feature 付き。

---

## Task 1: config 拡張 + pure staleness 判定（core crate）

**Files:**
- Modify: `crates/core/src/config.rs`（GmoFxSwapConfig + validate + テスト）
- Modify: `crates/core/src/swap.rs`（pure 関数 + テスト）
- Modify: `config/default.toml`（`[gmo_fx.swap]` セクション新設）

- [ ] **Step 1: 失敗するテストを書く（swap.rs の pure 関数）**

`crates/core/src/swap.rs` のテストモジュールに追加:

```rust
#[test]
fn staleness_boundary_is_exclusive() {
    use chrono::NaiveDate;
    let updated = NaiveDate::from_ymd_opt(2026, 7, 7).unwrap();
    // age == max_age_days ちょうどは stale ではない。超えたら stale。
    let on_limit = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(); // +35日
    let over_limit = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap(); // +36日
    assert!(!is_swap_rates_stale(updated, on_limit, 35));
    assert!(is_swap_rates_stale(updated, over_limit, 35));
}

#[test]
fn future_updated_on_is_not_stale() {
    use chrono::NaiveDate;
    let updated = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
    let today = NaiveDate::from_ymd_opt(2026, 7, 7).unwrap();
    assert!(!is_swap_rates_stale(updated, today, 35));
}
```

Run: `cargo test -p auto-trader-core staleness` → Expected: コンパイルエラー（関数未定義）

- [ ] **Step 2: pure 関数を実装**

`crates/core/src/swap.rs` に追加:

```rust
use chrono::NaiveDate;

/// swap rate 表が古いかの判定。`updated_on` から `max_age_days` を
/// **超えたら** stale（ちょうどは stale ではない）。未来日付は not stale。
///
/// スワップポイントは GMO が公表する配布値で API 取得手段が無いため、
/// config 手入力の rates が金利環境の変化で古くなる。この関数が日次ジョブ
/// から呼ばれ、超過時に SystemAlert で運用者へ更新を促す。
pub fn is_swap_rates_stale(updated_on: NaiveDate, today: NaiveDate, max_age_days: u32) -> bool {
    (today - updated_on).num_days() > i64::from(max_age_days)
}
```

Run: `cargo test -p auto-trader-core staleness` → Expected: PASS (2 tests)

- [ ] **Step 3: 失敗するテストを書く（config validate）**

`crates/core/src/config.rs` の既存 `parses_gmo_fx_swap_section` テスト（L713 付近）の近くに追加。既存テストの TOML 断片には `updated_on` が無いので、**既存テストの TOML に `updated_on = "2026-07-07"` を足して green を維持**した上で:

```rust
#[test]
fn swap_rates_without_updated_on_fail_validation() {
    // rates があるのに updated_on 無し → 起動拒否 (鮮度管理の起点が無い)
    let toml_str = /* 既存 parses_gmo_fx_swap_section と同じ构成で updated_on 行を省いた TOML */;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert!(config.validate().is_err());
}

#[test]
fn swap_updated_on_must_be_valid_date() {
    // updated_on = "not-a-date" → 起動拒否
    let toml_str = /* updated_on = "not-a-date" を含む TOML */;
    let config: AppConfig = toml::from_str(toml_str).unwrap();
    assert!(config.validate().is_err());
}

#[test]
fn empty_rates_do_not_require_updated_on() {
    // [gmo_fx.swap] 自体が無い既存 config は従来どおり valid
    // (gmo_fx_swap_defaults_to_empty_when_missing と同じ TOML で validate() Ok を確認)
}
```

TOML 断片は既存テスト（L713-731 / L748-762）の実物をコピーして最小改変すること。

- [ ] **Step 4: config を実装**

`GmoFxSwapConfig` を拡張:

```rust
#[derive(Debug, Deserialize, Clone, Default)]
pub struct GmoFxSwapConfig {
    #[serde(default)]
    pub rates: HashMap<String, SwapRateEntry>,
    /// rates を最後に GMO 公表スワップカレンダーと突合した日 ("YYYY-MM-DD")。
    /// rates が非空なら必須 (validate で強制)。
    #[serde(default)]
    pub updated_on: Option<String>,
    /// updated_on からこの日数を超えたら staleness アラート。
    #[serde(default = "default_swap_max_age_days")]
    pub max_age_days: u32,
}

fn default_swap_max_age_days() -> u32 {
    35 // 月次更新運用 + 猶予
}
```

（既存の derive/デフォルト挙動を壊さないこと。`GmoFxConfig` 側が `#[serde(default)]` で全体 default を要求しているなら `Default` 実装の整合を取る。）

`GmoFxSwapConfig` に validate を追加し、`AppConfig::validate()` から呼ぶ:

```rust
impl GmoFxSwapConfig {
    /// updated_on を NaiveDate として返す。rates 非空なのに未設定/不正なら Err。
    pub fn parsed_updated_on(&self) -> anyhow::Result<Option<chrono::NaiveDate>> {
        match &self.updated_on {
            None => Ok(None),
            Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(Some)
                .map_err(|e| anyhow::anyhow!("[gmo_fx.swap].updated_on '{s}' is not YYYY-MM-DD: {e}")),
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let parsed = self.parsed_updated_on()?;
        if !self.rates.is_empty() && parsed.is_none() {
            anyhow::bail!(
                "[gmo_fx.swap].updated_on is required when rates are set \
                 (staleness tracking needs a reference date)"
            );
        }
        if self.max_age_days == 0 {
            anyhow::bail!("[gmo_fx.swap].max_age_days must be > 0");
        }
        Ok(())
    }
}
```

`AppConfig::validate()` に `self.gmo_fx.swap.validate()?;` を追加。

- [ ] **Step 5: default.toml に `[gmo_fx.swap]` を新設**

まず WebFetch で GMO コイン 外国為替FX の公表スワップカレンダー（`https://coin.z.com/jp/fx/market/swap/` など。見つからなければ「GMOコイン 外国為替FX スワップポイント」で URL を探す）から **USD_JPY の直近の実レート（1 lot = 10,000 通貨あたりの円）** を確認し、実値で埋める:

```toml
# === GMO FX swap rates ===
# スワップポイントは GMO が日次公表する配布値で API 取得手段が無い (fxdocs 確認済み)。
# 月次で公式スワップカレンダーと突合し、rates と updated_on を更新すること。
# 符号: >0 = paper 払い / <0 = paper 受取 (crates/core/src/swap.rs 参照)。
# updated_on から max_age_days を超えると日次ジョブが SystemAlert を出す。
[gmo_fx.swap]
updated_on = "<確認日 YYYY-MM-DD>"
max_age_days = 35

[gmo_fx.swap.rates]
USD_JPY = { long = <実値>, short = <実値> }
```

公表ページが取得できない場合: rates は入れず `updated_on` も省略した**コメントアウトの雛形のみ**を置き、その旨を報告に明記する（Task 2 の「rates 未設定アラート」が運用者に設定を促す）。**架空の数値を実値のように書かないこと。**

- [ ] **Step 6: テスト + commit**

Run: `cargo test -p auto-trader-core` → PASS

```bash
git add -A
git commit -m "feat(core): swap-rate staleness tracking — updated_on/max_age_days + pure check"
```

---

## Task 2: 日次ジョブへの配線 + Runbook

**Files:**
- Modify: `crates/app/src/main.rs`（overnight/swap ジョブ ~L1996）
- Modify: `specs/design.md`（Runbook）

- [ ] **Step 1: アラート判定ヘルパを追加（app 側、テスト付き）**

`crates/app/src/main.rs` はテストしづらいので、判定は `crates/app/src/lib.rs` 配下の適切な場所（新規 `crates/app/src/swap_freshness.rs` + `pub mod swap_freshness;`）に置く:

```rust
//! GMO swap rate 表の鮮度チェック。日次スワップジョブから 1 日 1 回呼ばれ、
//! 「rates が古い」「rates 未設定なのに GMO paper 口座が存在する」を
//! 運用者向けアラート文言として返す。判定のみ (送信は main.rs)。

use auto_trader_core::config::GmoFxSwapConfig;
use auto_trader_core::swap::is_swap_rates_stale;
use chrono::NaiveDate;

/// アラートが必要なら本文を返す。不要なら None。
///
/// - rates 空 + GMO paper 口座あり → 未設定警告 (paper=live 近似が欠ける)
/// - rates あり + updated_on が max_age_days 超過 → 更新督促
pub fn swap_freshness_alert(
    cfg: &GmoFxSwapConfig,
    today: NaiveDate,
    has_gmo_paper_accounts: bool,
) -> Option<String> {
    if cfg.rates.is_empty() {
        if has_gmo_paper_accounts {
            return Some(
                "[gmo_fx.swap.rates] is EMPTY — GMO paper accounts are running WITHOUT \
                 swap simulation (paper PnL is optimistic). Fill rates + updated_on from \
                 the official swap calendar."
                    .to_string(),
            );
        }
        return None;
    }
    // validate 済みなので parse は成功するはずだが、防御的に None 扱い。
    let updated_on = cfg.parsed_updated_on().ok().flatten()?;
    if is_swap_rates_stale(updated_on, today, cfg.max_age_days) {
        let age = (today - updated_on).num_days();
        return Some(format!(
            "[gmo_fx.swap.rates] last verified {updated_on} ({age} days ago, limit {} days) — \
             re-check the official GMO swap calendar and update rates + updated_on",
            cfg.max_age_days
        ));
    }
    None
}
```

TDD: 先に 4 ケースのテスト（stale→Some / fresh→None / 空+GMO 口座あり→Some / 空+GMO 口座なし→None）を書き、落ちるのを確認してから実装。境界（ちょうど max_age_days）は Task 1 の pure 関数が担保済みなのでここでは 1 ケースで良い。

- [ ] **Step 2: main.rs 配線**

overnight/swap ジョブ（~L2002 の `tokio::spawn` 前）に notifier を clone して渡す（main.rs 内の Notifier 変数名を grep で確認し、既存の `let crypto_monitor_notifier = notifier.clone();` パターンに合わせる）。ジョブ内の「日付が変わった時」ブロック（`if today != last_date { ... }`）の中で、fee 適用と同じタイミングで 1 回だけ:

```rust
// swap rate 表の鮮度チェック (1 日 1 回、fee 適用と同時)。
let has_gmo_paper = accounts
    .iter()
    .any(|a| a.exchange == "gmo_fx" && a.account_type == "paper");
if let Some(body) =
    auto_trader::swap_freshness::swap_freshness_alert(&swap_config, today, has_gmo_paper)
{
    let ev = auto_trader_notify::NotifyEvent::SystemAlert(auto_trader_notify::SystemAlertEvent {
        title: "swap rates freshness".to_string(),
        account_name: "(config)".to_string(),
        exchange: auto_trader_core::types::Exchange::GmoFx,
        body,
    });
    let notifier = overnight_notifier.clone();
    tokio::spawn(async move {
        if let Err(e) = notifier.send(ev).await {
            tracing::warn!("swap freshness alert send failed: {e}");
        }
    });
}
```

`accounts` はブロック内で既に list_all 済みの変数を使う（実変数名に合わせる）。さらに起動時にも同じヘルパを 1 回呼び、Some なら `tracing::warn!` を出す（Slack は日次ジョブに任せ、起動時はログのみ）。

- [ ] **Step 3: Runbook 追記**

`specs/design.md` の運用 Runbook（Kill Switch 手動解除等が書いてある節）に追記:

```
- GMO スワップレート更新（月次）: GMO 公式のスワップカレンダーを確認し、
  `config/default.toml` の `[gmo_fx.swap.rates]` と `updated_on` を更新する。
  放置すると updated_on + max_age_days 超過で日次 SystemAlert が出る。
  スワップは API で取得できない公表値のため手動更新が唯一の手段。
```

- [ ] **Step 4: 全体テスト + commit**

Run: `./scripts/test-all.sh` → Expected: `ALL GREEN`

```bash
git add -A
git commit -m "feat(app): daily swap-rate freshness alert (stale / missing rates)"
```

---

## Self-Review（計画作成時に実施済み）

- スコープ: staleness 検知 + rates 未設定検知 + Runbook のみ。自動取得・スクレイピング・live 側 per-position swap 記帳（API 制約で不可能）は明示的にスコープ外。
- 型整合: `is_swap_rates_stale`（core/swap.rs）→ `swap_freshness_alert`（app）→ main.rs 配線の呼び出し連鎖で名前・引数一致を確認。
- 既存テストへの影響: `parses_gmo_fx_swap_section`（updated_on 追加が必要）、`gmo_fx_swap_defaults_to_empty_when_missing`（rates 空は従来どおり valid のまま）を明記済み。
- 不確実点: GMO 公表スワップカレンダーの URL / 取得可否 → 取得不能時のフォールバック（コメント雛形 + 未設定アラートに委ねる、架空値禁止）を Task 1 Step 5 に明記。
