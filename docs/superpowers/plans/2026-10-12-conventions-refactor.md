# コーディング規約への適合リファクタリング 実装計画

| 項目 | 内容 |
| --- | --- |
| 目的 | workspace 全体を `~/.claude/specs/conventions.md` と `conventions-rust.md` に適合させる作業の、順序・単位・各 run の完了条件を決める |
| 読者 | 各 run の spec を書くオーケストレーター、実装する kaneko、判定する reviewer |
| 扱う範囲 | 適合の順序、1 run の単位と上限、段階的に lint を有効にする方法、コメント禁止を強制するテストの仕様、crate ごとの作業の型 |
| 関連文書 | `~/.claude/specs/conventions.md`、`~/.claude/specs/conventions-rust.md`(判定基準)、`~/.claude/specs/ci-copilot-review.md`(PR の大きさの上限)、`~/.claude/specs/vibepod.md`(run の実行手順)、`CLAUDE.md`(commit 前の `./scripts/test-all.sh`) |

> **For agentic workers:** 各 run は vibepod コンテナで kaneko が実装し、reviewer が Stage 1 を判定する。Stage 2 と `./scripts/test-all.sh` はホストで行う。手順は `~/.claude/specs/vibepod.md` に従う。

**Goal:** 挙動を変えずに、全 crate のコードを規約に適合させ、規約の lint とテストを `-D warnings` で強制できる状態にする。

**Architecture:** 規約の lint を workspace に定義し、適合を終えた crate から順に `[lints] workspace = true` で有効にする。コメント禁止(KM-7、SC-9)は、`crates/core/tests/no_comments.rs` が走査対象の crate を列挙して強制し、適合を終えた crate を追加していく。crate ごとに「テストの分離」「コメントの除去」「分割と lint の有効化」の 3 種類の run を回す。

**Tech Stack:** Rust 1.99.0、clippy(`clippy.toml`)、cargo workspace lints。

**Spec:** `~/.claude/specs/conventions.md`、`~/.claude/specs/conventions-rust.md`

## Global Constraints

- 挙動を変えない。公開 API、DB スキーマ、設定ファイルの形式、ログの key 名は変えない(KM-4)。例外は、テスト外の `unwrap` / `expect` の除去で戻り値が `Result` になる場合だけで、呼び出し元を同じ変更で直す
- 変更前から存在する KM-9 の違反のうち、型や戦略の全体を統合しないと解消できないもの(例: `donchian_trend` と `donchian_trend_evolve` の 2 戦略)は、この計画の対象外とし、Issue #124 で扱う。同じ処理を持つ小さな関数(判定、変換、ログ)の重複は、この計画の run で 1 か所に集約する
- 変更前から存在する FF-2 と FF-6 の違反(既定値での補正、警告だけ出して処理を続ける経路、`let _ =` での読み捨て)のうち、直すと挙動が変わるものは、この計画の対象外とする。run の reviewer は、差分が持ち込んだ違反だけを判定の対象にし、変更前から存在する違反は Issue #118 に箇所を追記して報告する。これらの修正は、箇所ごとに失敗時の仕様(周期ジョブが止まるか、その項目だけを飛ばして呼び出し元へ返すか)を利用者が決めてから、別の計画で行う
- 依存 crate を追加しない(KM-1)
- 1 run の本体コード(テスト・コメント・空行を除く追加と変更の行数)は 500 行以内、合計の追加は 1,500 行以内(`ci-copilot-review.md` §2.1)
- 各 run の後、ホストで `./scripts/test-all.sh` が ALL GREEN
- commit はホストのオーケストレーターが行う(コンテナ内ではコミットしない。全体テストがコンテナの 1 コマンド 10 分の制限を超えるため)
- `rustfmt` の `group_imports` は stable で使えないため、`use` の 3 グループ分け(SC-3)は手で揃える

## Review Focus

- コメントの検出が、文字列リテラルの中の `//`(URL など)を誤って数えない(D-9)
- `#[cfg(test)] mod tests;` に分離したテストが、`cargo test -p <crate> --lib` で元と同じ件数だけ実行される
- doc コメントを除いたとき、呼び出し側が依存していた仕様(受け付けない入力)が、テストとして残る
- `unwrap` / `expect` を `Result` に変えたとき、呼び出し元がエラーを `let _ =` で捨てていない(FF-6)
- 分割したファイルの `pub` の可視性が、分割前と同じ範囲に収まる(新しい `pub` を作らない)

---

## 決定

- D-1 lint の定義: `Cargo.toml` の `[workspace.lints.clippy]` に `too_many_lines`、`cognitive_complexity`、`fn_params_excessive_bools`、`unwrap_used`、`expect_used` を `"warn"` で置き、`./scripts/test-all.sh` の `-D warnings` で失敗させる。しきい値は `clippy.toml` の `too-many-lines-threshold = 80`、テストの `unwrap` / `expect` は `allow-unwrap-in-tests = true` と `allow-expect-in-tests = true` で許す
- D-2 段階的な有効化: crate が適合を終えた run で、その crate の `Cargo.toml` に `[lints] workspace = true` を加え、`crates/core/tests/no_comments.rs` の `CHECKED_CRATES` にその crate のディレクトリ名を加える。未適合の crate には lint を適用しない
- D-3 crate の順序: core → notify → vegapunk-client → macro-analyst → backtest → db → executor → market → strategy → app → sim → integration-tests(依存の少ない順)
- D-4 テストの置き場所: `src/<module>.rs` の末尾の `#[cfg(test)] mod <name> { ... }` は、`src/<module>.rs` に `#[cfg(test)] mod tests;` を置き、`src/<module>/tests.rs` に移す。1 ファイルに複数のテストモジュールがある場合は、1 つの `tests.rs` の中で `mod <name> { ... }` として残す。`tests.rs` の `use` も SC-3 の順(標準ライブラリ → サードパーティ → 同じ crate)に並べ、`use super::*;` は同じ crate のグループに置く
- D-5 ログ(SC-12): 共通のログ関数は `tracing` のマクロとし、出力の形は `crates/app/src/main.rs` の subscriber の設定 1 か所で決める。本文は小文字で始め、末尾にピリオドを付けず、識別子・件数は構造化フィールド(`key = value`)で渡す。本文の書き換えは、その crate の「コメントの除去」run で行う
- D-6 doc コメント(SC-9)の内容の移し先: 受け付けない入力の説明はテスト(FF-5)に、理由の説明は関数名・定数名に移す。移し先が無い説明(型の用途の説明など)は削除する
- D-7 run の 3 種類と順序(crate ごと): R-a テストの分離と `use` の整列 → R-b コメントの除去と SC-6・SC-12 への文言の統一 → R-c ファイルと関数の分割、`unwrap` / `expect` の除去、lint の有効化。R-a と R-b は本体の追加行が少ないため 1 run にまとめてよい
- D-8 `scripts/test-all.sh` の変更: 先頭に `cargo build --workspace --locked` を加え、clippy と test に `--locked` を付ける
- D-9 コメントの検出範囲: Phase 0 のテストは行頭が `//` または `/*` の行だけを検出する。core の R-b で、文字列リテラル(`"..."`、`r"..."`、`r#"..."#`)の外にある `//` と `/*` をすべて検出する `line_has_comment` に置き換え、行末コメントも対象にする。`// SAFETY:` で始まる行だけを許す

---

## Phase 0: 強制の仕組み(1 run)

### Task 0-1: lint の定義と `clippy.toml`

**Files:**
- Modify: `Cargo.toml`(末尾に追加)
- Create: `clippy.toml`

- [ ] **Step 1: `Cargo.toml` の末尾に追加する**

```toml
[workspace.lints.clippy]
too_many_lines = "warn"
cognitive_complexity = "warn"
fn_params_excessive_bools = "warn"
unwrap_used = "warn"
expect_used = "warn"
```

- [ ] **Step 2: `clippy.toml` を作る**

```toml
too-many-lines-threshold = 80
allow-unwrap-in-tests = true
allow-expect-in-tests = true
```

- [ ] **Step 3: どの crate にも `[lints] workspace = true` を加えていない状態で、`cargo clippy --workspace --all-targets --locked -- -D warnings` が通ることを確認する**

Expected: 既存の警告 0 件(lint はまだどの crate にも適用されていない)。

### Task 0-2: コメント禁止を強制するテスト

**Files:**
- Create: `crates/core/tests/no_comments.rs`

**Interfaces:**
- Produces: `CHECKED_CRATES: &[&str]`(走査する `crates/<name>` の一覧。Phase 1 以降の run が追記する)

- [ ] **Step 1: テストを書く(`CHECKED_CRATES` は空で始める)**

```rust
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

const CHECKED_CRATES: &[&str] = &[];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rust_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

fn is_forbidden_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("// SAFETY:") {
        return false;
    }
    trimmed.starts_with("//") || trimmed.starts_with("/*")
}

fn comment_lines_in(file: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let text = fs::read_to_string(file)?;
    Ok(text
        .lines()
        .enumerate()
        .filter(|(_, line)| is_forbidden_comment_line(line))
        .map(|(i, line)| format!("{}:{}: {}", file.display(), i + 1, line.trim()))
        .collect())
}

#[test]
fn checked_crates_have_no_comment_lines_in_src() -> Result<(), Box<dyn Error>> {
    let root = workspace_root();
    let mut violations = Vec::new();
    for crate_dir in CHECKED_CRATES {
        let mut files = Vec::new();
        collect_rust_files(&root.join("crates").join(crate_dir).join("src"), &mut files)?;
        for file in &files {
            violations.extend(comment_lines_in(file)?);
        }
    }
    assert!(violations.is_empty(), "comment lines found:\n{}", violations.join("\n"));
    Ok(())
}

#[test]
fn line_comment_is_forbidden() {
    assert!(is_forbidden_comment_line("    // explain"));
}

#[test]
fn doc_comment_is_forbidden() {
    assert!(is_forbidden_comment_line("/// doc"));
    assert!(is_forbidden_comment_line("//! module doc"));
}

#[test]
fn block_comment_start_is_forbidden() {
    assert!(is_forbidden_comment_line("/* block */"));
}

#[test]
fn safety_comment_is_allowed() {
    assert!(!is_forbidden_comment_line("    // SAFETY: fd is owned"));
}

#[test]
fn url_inside_string_literal_is_not_a_comment() {
    assert!(!is_forbidden_comment_line(r#"let url = "https://example.com";"#));
}
```

- [ ] **Step 2: `cargo test -p auto-trader-core --test no_comments` が通ることを確認する**

Expected: 7 passed(Unix では symlink を辿らないことのテストを含めて 8 passed)。

- [ ] **Step 3: `CHECKED_CRATES` を一時的に `&["core"]` にして実行し、`checked_crates_have_no_comment_lines_in_src` が現在の `crates/core/src` のコメントを列挙して失敗することを確認し、`&[]` に戻す(RH-8)**

Expected: FAIL。失敗出力の先頭 20 行を実装報告に含める。

### Task 0-3: `scripts/test-all.sh` に `--locked` と build を加える

**Files:**
- Modify: `scripts/test-all.sh`(「静的チェック」の直前と、clippy・test の各行)

- [ ] **Step 1: 「静的チェック」の節の前に、次を加える**

```bash
step "cargo build --workspace --locked"
cargo build --workspace --locked
ok "build passed"
```

- [ ] **Step 2: clippy と test の 3 コマンドに `--locked` を付ける**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --lib --bins --tests --locked
cargo test --workspace --doc --locked
```

- [ ] **Step 3: `CLAUDE.md` の「commit 前のテスト実行」の手順一覧に、`cargo build --workspace --locked` を 1 番目として加え、以降の番号をずらす**

### Phase 0 の完了条件

- `./scripts/test-all.sh` が ALL GREEN
- reviewer の Stage 1〜2 が PASS または CONDITIONAL PASS

---

## Phase 1 以降: crate ごとの run

D-3 の順に、crate ごとに次の run を回す。各 run の spec は `.superpowers/runs/YYYY-MM-DD-conv-<crate>-<a|b|c>.md` に書き、この計画の該当節を参照する。

### Run R-a: テストの分離と `use` の整列

対象: その crate の `src/**/*.rs` のうち、`#[cfg(test)] mod ... { ... }` を含むファイルすべて。

- [ ] **Step 1: 対象ファイルを列挙する**

Run: `grep -ln '#\[cfg(test)\]' crates/<crate>/src -r`

- [ ] **Step 2: 変更前のテスト件数を記録する**

Run: `cargo test -p <package> --lib 2>&1 | grep 'test result'`

- [ ] **Step 3: ファイルごとに、D-4 に従って `src/<module>/tests.rs` へ移す**

`src/<module>.rs` の末尾は次の形にする。

```rust
#[cfg(test)]
mod tests;
```

`src/<module>/tests.rs` の先頭は次の形にする。元のテストモジュールが 1 つなら中身をそのまま置き、複数なら `mod <元の名前> { use super::*; ... }` として並べる。

```rust
use super::*;
```

- [ ] **Step 4: 本体ファイルの `use` を、標準ライブラリ・サードパーティ・同じ crate の 3 グループに分け、この順に並べる(SC-3)**

- [ ] **Step 5: 変更後のテスト件数が Step 2 と同じであることを確認する**

Run: `cargo test -p <package> --lib 2>&1 | grep 'test result'`

- [ ] **Step 6: `cargo fmt --all --check` と `cargo clippy -p <package> --all-targets --locked -- -D warnings` を通す**

### Run R-b: コメントの除去と文言の統一

対象: その crate の `src/**/*.rs`(`tests.rs` を含む)。

- [ ] **Step 1: コメント行を列挙する**

Run: `grep -nE '^\s*(//|/\*)' -r crates/<crate>/src | grep -v 'SAFETY:'`

- [ ] **Step 2: 行ごとに、D-6 に従って移し先を決めて移す。移し先の無いものは削除する**

- 受け付けない入力の説明 → 対象ファイルの `tests.rs` に、その入力がエラーになるテスト(FF-5)
- 理由の説明(順序への依存、外部仕様の癖、対策) → 理由を名前に持つ関数・定数・型
- 型や関数の用途の説明、作業の経緯 → 削除

- [ ] **Step 3: エラーメッセージを SC-6 の 3 定型(`invalid <対象>: <理由>`、`unsupported <対象>: <値>`、`failed to <動作>: <原因>`)に揃える。小文字で始め、末尾にピリオドを付けない。`#[source]` と `{source}` の両方で原因を出していれば一方にする**

- [ ] **Step 4: `tracing` の本文を D-5 に揃える。本文に埋め込んだ識別子・件数は構造化フィールドに移す。既存のフィールドの key 名は変えない**

- [ ] **Step 5: `crates/core/tests/no_comments.rs` の `CHECKED_CRATES` にこの crate のディレクトリ名を加え、`cargo test -p auto-trader-core --test no_comments` が通ることを確認する**

- [ ] **Step 6: `cargo test -p <package> --lib` の件数が R-a の後と同じか、FF-5 で加えた分だけ増えていることを確認する**

### Run R-c: 分割、`unwrap` / `expect` の除去、lint の有効化

対象: その crate 全体。

- [ ] **Step 1: 500 行を超える本体ファイルと、80 行を超える関数を列挙する**

80 行を超える関数とテスト外の `unwrap` / `expect` は clippy で列挙する。

Run: `cargo clippy -p <package> --all-targets --locked -- -W clippy::too_many_lines -W clippy::cognitive_complexity -W clippy::fn_params_excessive_bools -W clippy::unwrap_used -W clippy::expect_used 2>&1 | grep -E '^(warning|error)' | sort | uniq -c`

500 行を超える本体ファイルは行数で列挙する(`tests.rs` は本体に含めない)。

Run: `find crates/<crate>/src -name '*.rs' -not -name tests.rs -exec awk 'END { if (NR > 500) print NR, FILENAME }' {} \;`

- [ ] **Step 2: 500 行を超える本体ファイルを、責務ごとに `src/<module>/<responsibility>.rs` に分ける(KM-5、SC-2)。分割先の型と関数の可視性は分割前と同じ範囲にし、`pub(crate)` より広くしない**

- [ ] **Step 3: 80 行を超える関数を、入力の検証・本処理・結果の組み立てのように段階で分ける。1 回しか呼ばれない補助関数も、名前が段階を表すなら作ってよい**

- [ ] **Step 4: テスト外の `unwrap` / `expect` を除く。失敗し得るものは `Result` で呼び出し元に返し、FF-6 に従って呼び出し元でも捨てない。失敗し得ないもの(定数の parse など)は `const` か、起動時に 1 回検証して保持する値に置き換える**

- [ ] **Step 5: `crates/<crate>/Cargo.toml` に次を加える**

```toml
[lints]
workspace = true
```

- [ ] **Step 6: `cargo clippy -p <package> --all-targets --locked -- -D warnings` と `cargo test -p <package>` を通す**

### crate ごとの対象(2026-10-12 時点の計測)

| crate | package | src の行数 | テストを含むファイル数 | 500 行超の本体ファイル | 分割の方針 |
| --- | --- | --- | --- | --- | --- |
| core | auto-trader-core | 2,935 | 8 | `config.rs`(本体 548 行) | `FeedWatchdogConfig` と `NOTIFY_SEND_TIMEOUT_SECS`、`MAX_NOTIFY_RETRY_ATTEMPTS`(50〜235 行)を `config/feed_watchdog.rs` に移す |
| notify | auto-trader-notify | 482 | 1 | なし | なし |
| vegapunk-client | auto-trader-vegapunk | 163 | 0 | なし | なし |
| macro-analyst | auto-trader-macro-analyst | 293 | 0 | なし | なし |
| backtest | auto-trader-backtest | 615 | 計測して決める | 計測して決める | R-c の Step 1 で決める |
| db | auto-trader-db | 3,842 | 計測して決める | `trading_accounts.rs`、`trades.rs`、`dashboard.rs` | R-c の Step 1 で決める |
| executor | auto-trader-executor | 3,373 | 計測して決める | `trader.rs` | R-c の Step 1 で決める |
| market | auto-trader-market | 7,356 | 計測して決める | `bitflyer_private.rs`、`gmo_fx_private.rs`、`bitflyer.rs`、`oanda_private.rs`、`price_store.rs` | R-c の Step 1 で決める |
| strategy | auto-trader-strategy | 4,652 | 計測して決める | `fx_new.rs`、`squeeze_momentum.rs`、`donchian_trend_evolve.rs`、`bb_mean_revert.rs`、`donchian_trend.rs` | R-c の Step 1 で決める |
| app | auto-trader | 10,095 | 計測して決める | `main.rs`、`feed_watchdog.rs`、`startup_reconcile.rs`、`weekly_batch.rs`、`startup.rs` | R-c の Step 1 で決める |
| sim | auto-trader-sim | 14,206 | 計測して決める | `script.rs`、`cli.rs`、`indicators.rs`、`sweep.rs`、`engine.rs`、`eval.rs`、`fetch.rs`、`series.rs`、`benchmark.rs` | R-c の Step 1 で決める |
| integration-tests | auto-trader-integration-tests | 21,840 | 計測して決める | なし(`tests/` 配下は対象外) | なし |

「計測して決める」の列は、その crate の R-a の spec を書くときに、この表を計測値で置き換える。1 run の本体の追加が 500 行を超える見込みの crate(app、sim、market、strategy)は、R-c をファイル単位の複数 run に分ける。

### 各 run の完了条件

- run の対象 crate で `cargo fmt --all --check`、`cargo clippy -p <package> --all-targets --locked -- -D warnings`、`cargo test -p <package>` が通る
- ホストで `./scripts/test-all.sh` が ALL GREEN
- reviewer の Stage 1(コンテナ)と Stage 2(ホスト)が PASS または CONDITIONAL PASS
- 本体の追加と変更が 500 行以内

## 全体の完了条件

- 全 crate の `Cargo.toml` に `[lints] workspace = true` がある
- `CHECKED_CRATES` に全 crate がある
- `./scripts/test-all.sh` が ALL GREEN
