# GMO FX EUR_USD を 4 戦略で有効化する Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `config/default.toml` の 4 つの `[[strategies]]` エントリの `pairs` array に `"EUR_USD"` を加え、GMO FX の EUR_USD で paper トレードが走るようにする。コード変更なし。

**Architecture:** 純粋な config 変更。market feed / pair_config / strategy params は既に EUR_USD 対応済 (各戦略の `on_price` が `pairs` array に基づく early-return をしているだけ)。`pairs` に文字列を 1 つ足せば該当戦略が EUR_USD の PriceEvent を処理対象にする。

**Tech Stack:** TOML 設定のみ。Rust コード変更なし。

---

## Required Test Command (各タスクの DoD)

CLAUDE.md 必須:

```bash
./scripts/test-all.sh
```

`ALL GREEN` が出るまで次タスクへ進まない。`Bash(git commit*)` PreToolUse hook も同じスクリプトを発火するので、commit ステップが失敗したら自動的にブロックされる。

---

## File Structure

変更:
- `config/default.toml` (4 行修正、各 `[[strategies]]` の `pairs` に `"EUR_USD"` を追加)

新規ファイルなし。

---

## Task 0: Baseline 確認

**Files:** なし

- [ ] **Step 1: スクリプトで全段階緑を確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。失敗があれば計画着手前に修正。

- [ ] **Step 2: ブランチ確認**

```bash
git branch --show-current
```

Expected: `feat/gmo-fx-multi-pair`。spec commit (`docs/superpowers/specs/2026-05-16-gmo-fx-eur-usd-enablement-design.md`) が既にこのブランチに乗っている。

---

## Task 1: 4 戦略の pairs に EUR_USD を追加

**Files:**
- Modify: `config/default.toml:101-123` (4 つの `[[strategies]]` エントリ)

- [ ] **Step 1: 現状の確認**

```bash
grep -nA3 "^\[\[strategies\]\]" config/default.toml | head -30
```

Expected: 5 つの `[[strategies]]` エントリ (swing_llm_v1 は `enabled = false` で USD_JPY + EUR_USD 既に登録済、残り 4 つは `["FX_BTC_JPY", "USD_JPY"]`)。

- [ ] **Step 2: 4 戦略の pairs に "EUR_USD" を追加**

`config/default.toml` で 4 箇所を以下の通り編集 (どれも `pairs = ["FX_BTC_JPY", "USD_JPY"]` → `pairs = ["FX_BTC_JPY", "USD_JPY", "EUR_USD"]`):

```toml
[[strategies]]
name = "bb_mean_revert_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY", "USD_JPY", "EUR_USD"]

[[strategies]]
name = "donchian_trend_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY", "USD_JPY", "EUR_USD"]

[[strategies]]
name = "donchian_trend_evolve_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY", "USD_JPY", "EUR_USD"]

[[strategies]]
name = "squeeze_momentum_v1"
enabled = true
mode = "paper"
pairs = ["FX_BTC_JPY", "USD_JPY", "EUR_USD"]
```

- [ ] **Step 3: 編集後の確認**

```bash
grep -c "EUR_USD" config/default.toml
```

Expected: 8 以上 (`[pair_config.EUR_USD]` 1 行 + `[pairs] fx` 1 行 + swing_llm_v1 1 行 + 今回追加 4 行 = 7、コメントや既存出現を含めて ≥ 8)。

- [ ] **Step 4: TOML パースが壊れていないことを confirm**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p auto-trader-core config 2>&1 | tail -5
```

Expected: `auto_trader_core` の config パーステストが全て pass。`config/default.toml` を parse する unit test が含まれる場合はそこで構文エラーを検出。

- [ ] **Step 5: 全体 test-all.sh で regression なし確認**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`。

- [ ] **Step 6: Commit**

```bash
git add config/default.toml
git commit -m "feat(config): enable EUR_USD on 4 GMO FX paper strategies (bb / donchian / donchian_evolve / squeeze)"
```

(hook が `test-all.sh` を発火、ALL GREEN を確認)

---

## Task 2: 最終検証 + PR 作成 + Copilot review

**Files:** なし

- [ ] **Step 1: 残骸 grep**

```bash
git diff main...HEAD --stat
```

Expected: 出力は spec + plan + config の 3 ファイルのみ (合計 + ~10 行)。コード差分なし。

- [ ] **Step 2: フル test-all.sh**

```bash
./scripts/test-all.sh
```

Expected: `ALL GREEN`、warning 0。

- [ ] **Step 3: simplify skill (scope 小さいので軽く確認のみ)**

config 1 行追加が 4 箇所なので simplify 3 並列 agent は overkill。inline で:
- reuse: pairs 配列に `EUR_USD` を文字列で 4 回書いているが、DRY 化のために共通変数を導入するのは TOML では不可。維持。
- quality: コメントは既存 (前 PR で書かれた `pairs controls which price events the strategy processes`) で充分。追加コメント不要。
- efficiency: config 起動時 1 回のパースのみ、ホットパス影響ゼロ。

simplify 自体は skip。

- [ ] **Step 4: code-review skill 経由で codex review**

CLAUDE.md の規律通り `codex:codex-rescue` で reviewer.md ペルソナの review を回す。PR #86 / #87 で permission 問題があったので self-review + Copilot review でフォロー (ユーザーに確認)。

- [ ] **Step 5: PR 作成**

```bash
gh pr create --base main --head feat/gmo-fx-multi-pair \
  --title "feat(config): GMO FX EUR_USD を 4 paper 戦略で有効化 (paper=live 契約 3/N)" \
  --body "<PR description>"
```

PR description には:
- spec へのリンク (`docs/superpowers/specs/2026-05-16-gmo-fx-eur-usd-enablement-design.md`)
- 変更が config 4 行のみ
- `donchian_trend_evolve_v1` は GMO FX paper account 不在のため signal が dispatch 段階で skip される (USD_JPY と同じ現状を維持、別タスクで account 追加)
- merge 後 24-48h の paper 運用ログで EUR_USD signal / trade row / エラーログを確認することを記載
- 契約違反項目の対応状況 (#3 EUR_USD → 実装、残り 5 項目は別 PR)

- [ ] **Step 6: Copilot review ループ**

PR 作成後、`gh pr edit <PR#> --add-reviewer copilot-pull-request-reviewer` で Copilot 起動。
config 変更のみで Critical 級指摘はほぼ出ないはず。stale 再提起 / minor の suggestion 程度なら ack して止める。最大 3 ラウンド。

---

## Spec Coverage Check

| spec セクション | 対応タスク |
|---|---|
| 4 戦略の `pairs` に EUR_USD 追加 | Task 1 |
| TOML パース regression なし確認 | Task 1 Step 4 |
| 全体 test-all.sh ALL GREEN | Task 1 Step 5, Task 2 Step 2 |
| `donchian_trend_evolve_v1` の account 不在を別タスクへ | Task 2 Step 5 (PR description で明記) |
| merge 後の paper 運用観察ポイント | Task 2 Step 5 (PR description で明記) |
| スコープ外 (live 切替 / 専用 params 最適化 / backtest 再実行) | 全 Task で対象外 |
