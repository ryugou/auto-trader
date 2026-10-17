# Rustdoc and Doc Lints Conformance Implementation Plan

| Item | Content |
| --- | --- |
| Purpose | Decide the order, units and completion conditions of the runs that bring the workspace in line with `conventions-rust.md` SC-9 and SC-11 (Issue #170) |
| Audience | The orchestrator who writes each run spec, kaneko who implements it, reviewer who judges it |
| Scope | The comment-test change that allows rustdoc, the rustdoc runs per crate, the enablement of the doc lints, the policy for the remaining integration-tests run of #114 |
| Related documents | `~/.claude/specs/conventions-rust.md` (SC-9, SC-11, KM-3, KM-7: the criteria), `docs/superpowers/plans/2026-10-12-conventions-refactor.md` (the #114 plan; its D-1, D-3, D-6 and D-9 reference this plan), `CLAUDE.md` (`./scripts/test-all.sh` before commit) |

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every crate in `CHECKED_CRATES` carries the rustdoc that SC-9 requires, and `missing_docs`, `clippy::missing_errors_doc` and `clippy::missing_panics_doc` fail the build with `-D warnings`.

**Architecture:** First change `crates/core/tests/no_comments.rs` so that rustdoc lines pass. Then add rustdoc crate by crate, verifying each run with the doc lints passed on the command line. The last run adds the doc lints to `[workspace.lints]`, which turns them on for the 11 crates that already inherit workspace lints.

**Tech Stack:** Rust 1.99.0, rustc `missing_docs`, clippy `missing_errors_doc` / `missing_panics_doc`.

**Spec:** `~/.claude/specs/conventions-rust.md` SC-9 and SC-11.

## Global Constraints

- Rustdoc follows every bullet of SC-9 and KM-7 in `conventions-rust.md`; this plan adds only the constraints below.
- Do not change code: no signature, visibility, `pub` / `pub(crate)` narrowing, log, string or test change. A run changes only rustdoc lines and `#![doc = "..."]` attributes, plus these named exceptions: Task 1's change to `crates/core/tests/no_comments.rs`, and Task 5's changes to `Cargo.toml` and to the attributes of `pub mod proto` in `crates/vegapunk-client/src/lib.rs`.
- Rustdoc contains no fenced or indented code block (so `cargo test --doc` gains no doctest) and no intra-doc link (`[`Item`]`); name items in plain backticks.
- A module is documented with `///` on its `pub mod <name>;` declaration, never with `//!` inside the module file.
- Each run adds at most 500 rustdoc lines, counted with `git diff -- crates | grep -E '^\+' | grep -vE '^\+\+\+' | grep -cE '^\+\s*(///|#!\[doc)'` (the runs create no new file). A run that would exceed it is split by file in its spec.
- After each run, `./scripts/test-all.sh` is ALL GREEN on the host.

## Decisions

- D-R1 Order: the comment test changes first (Task 1), the doc lints are added to `[workspace.lints]` last (Task 5). Until Task 5 the lints are not active, so each run proves its crate with `DOCLINT` below.
- D-R2 `DOCLINT` = `cargo clippy -p <package> --all-targets --locked --no-deps -- -D warnings -W missing_docs -W clippy::missing_errors_doc -W clippy::missing_panics_doc`. A crate is done when `DOCLINT` exits 0. `--no-deps` keeps the lint flags off path dependencies such as core, which would otherwise fail first while they still lack rustdoc.
- D-R3 Test targets: `missing_docs` reports the crate root of a file under `tests/` that has no crate doc. Give each such file one `#![doc = "..."]` line in the run of its crate. Other comments under `tests/` stay out of scope, as in the #114 plan.
- D-R4 Generated code: the tonic code that `tonic::include_proto!("graphrag")` pulls into `pub mod proto` is not documented. Task 5 adds `#[expect(missing_docs, clippy::missing_errors_doc, reason = "tonic-generated code for proto/graphrag.proto")]` to `pub mod proto` in `crates/vegapunk-client/src/lib.rs`. The existing `#[allow]` attributes on that module stay.
- D-R5 `# Errors` states, per error path a caller can tell apart, the condition in one bullet (example: `/// - Returns an error if the database query fails.`). `# Panics` states the condition that panics.
- D-R6 integration-tests: the #114 runs for `crates/integration-tests` (R-a, R-b and R-c per D-7 of the #114 plan) start after Task 5. Its measured load: 242 `///` lines and 51 `//!` lines in `src/`, and 126 hand-written `DOCLINT` hits (94 `missing_docs`, 5 `missing_errors_doc`, 27 `missing_panics_doc`). Its generated code reports only `missing_docs`, so `pub mod proto` in its `src/lib.rs` gets `#[expect(missing_docs, reason = "tonic-generated code for proto/graphrag.proto")]` instead of the D-R4 attribute, added in its R-c Step 5 together with `[lints] workspace = true`.

## Measured targets (main da5a7f3, `.superpowers/logs/I170-doclints*.log`)

Hits from `DOCLINT` without `-D warnings`, generated code excluded. An undocumented function that returns `Result` counts once under `missing_docs` and once under `missing_errors_doc`. Estimated rustdoc lines = `missing_docs` hits + test-target crate roots + 4 per `# Errors` / `# Panics` section.

| crate | missing_docs (lib and bin) | missing_errors_doc | missing_panics_doc | test-target crate roots | estimated lines |
| --- | --- | --- | --- | --- | --- |
| notify | 45 | 2 | 0 | 0 | 53 |
| vegapunk-client | 6 (lib root, `build.rs` root, `pub mod proto`, 3 items) | 1 | 0 | 0 | 9 (`pub mod proto` is covered by D-R4) |
| macro-analyst | 29 | 6 | 0 | 0 | 53 |
| backtest | 20 | 1 | 0 | 0 | 24 |
| executor | 27 | 0 | 0 | 1 (`tests/risk_gate_test.rs`) | 28 |
| strategy | 30 | 1 | 0 | 0 | 34 |
| core | 286 | 7 | 0 | 1 (`tests/no_comments.rs`) | 315 |
| db | 209 | 64 | 0 | 1 (`tests/migration_test.rs`) | 466 |
| market | 209 | 17 | 1 | 0 | 281 |
| app | 116 (incl. `main.rs` root) | 2 | 0 | 0 | 124 |
| sim | 310 (incl. `main.rs` root) | 26 | 0 | 4 (`tests/{cli,data,fetch,store}_test.rs`) | 418 |

## Review Focus

- A line with `////`, `///text` (no space), `/// ` after code on the same line, `//!`, `/**` or `/*!` still fails the comment test.
- Rustdoc on a `pub(crate)` or private item (KM-7): the comment test cannot see it, so reviewer checks every added `///` against the `DOCLINT` hit list of the run.
- A `# Errors` section that names an error path the function does not have, or misses one it has.
- An indented line inside rustdoc that rustdoc would compile as a doctest.
- The market lib-test target reports one more hit than the lib (228 vs 227); the run of market resolves every hit of `--all-targets`, not only the lib's.

---

### Task 1: Comment test allows rustdoc lines (Run D1, part 1)

**Files:**
- Modify: `crates/core/tests/no_comments.rs` (`comment_line_numbers`, tests at the end)

**Interfaces:**
- Produces: the rule "a line whose text after leading whitespace is `///` followed by a space or the end of the line is not a comment violation"; every later task relies on it.

- [ ] **Step 1: Replace `doc_comment_is_forbidden` with these tests**

```rust
#[test]
fn rustdoc_line_is_allowed() {
    assert!(!has_comment("/// Returns the price."));
    assert!(!has_comment("    ///"));
    assert!(!has_comment("    /// # Errors"));
}

#[test]
fn inner_doc_comment_is_forbidden() {
    assert!(has_comment("//! module doc"));
    assert!(has_comment("/*! module doc */"));
}

#[test]
fn malformed_rustdoc_is_forbidden() {
    assert!(has_comment("///no space"));
    assert!(has_comment("//// four slashes"));
    assert!(has_comment("/** block doc */"));
    assert!(has_comment("let x = 1; /// trailing"));
}
```

- [ ] **Step 2: Run `cargo test -p auto-trader-core --test no_comments --locked`; expect `rustdoc_line_is_allowed` to FAIL and the other two to pass**

- [ ] **Step 3: In `comment_line_numbers`, skip a `//` that starts a rustdoc line, next to the existing `is_safety_comment_at` check**

Add `fn is_rustdoc_line_at(text: &str, slashes: usize) -> bool`: true when the text from the line start to `slashes` is whitespace only, `text[slashes..]` starts with `///`, and the next byte is a space, `\n`, `\r`, or the end of the text.

- [ ] **Step 4: Run the same command; expect all tests to pass**

### Task 2: Rustdoc for the small crates (Run D1, part 2)

**Files:**
- Modify: `crates/{notify,vegapunk-client,macro-analyst,backtest,executor,strategy}/src/**/*.rs`, `crates/vegapunk-client/build.rs`, `crates/executor/tests/risk_gate_test.rs`

**Interfaces:**
- Consumes: Task 1's rule.

- [ ] **Step 1: For each package, list hits: `DOCLINT` without `-D warnings`, output to `.superpowers/logs/D<n>-<crate>-before.log`, where `D<n>` is the run**
- [ ] **Step 2: Add rustdoc to every hit under the Global Constraints and D-R3 / D-R5; leave `pub mod proto` without rustdoc (D-R4)**
- [ ] **Step 3: Run `DOCLINT` for each of the 6 packages; expect exit 0 except vegapunk-client, whose only remaining hits are `pub mod proto` and the code inside it**
- [ ] **Step 4: Run `cargo test -p auto-trader-core --test no_comments --locked` and `./scripts/test-all.sh`; expect pass and ALL GREEN**
- [ ] **Step 5: Commit Tasks 1 and 2 as Run D1, one PR**

### Task 3: Rustdoc for core, then db (Runs D2 and D3)

Run D2 (core) and Run D3 (db), each one PR. Each applies Task 2 Steps 1, 2 and 4 to its crate and its test-target file (D-R3); its Step 3 is `DOCLINT` for that package, expected exit 0. Commit each as its run (D2, D3), one PR.

### Task 4: Rustdoc for market and app (Run D4)

Task 2 Steps 1, 2 and 4 applied to both crates; `crates/app/src/main.rs` gets `#![doc = "..."]`. Its Step 3 is `DOCLINT` for `auto-trader-market` and `auto-trader`, expected exit 0, including the market lib-test hit. Commit as Run D4, one PR.

### Task 5: Rustdoc for sim and doc lint enablement (Run D5)

**Files:**
- Modify: `crates/sim/src/**/*.rs`, `crates/sim/tests/{cli,data,fetch,store}_test.rs`, `crates/vegapunk-client/src/lib.rs` (D-R4), `Cargo.toml` (`[workspace.lints]`)

One PR.

- [ ] **Step 1: Task 2 Steps 1 and 2 applied to sim; `crates/sim/src/main.rs` gets `#![doc = "..."]`; expect `DOCLINT` exit 0 for `auto-trader-sim`**
- [ ] **Step 2: Add to the root `Cargo.toml`**

```toml
[workspace.lints.rust]
missing_docs = "warn"
```

and, in the existing `[workspace.lints.clippy]`, `missing_errors_doc = "warn"` and `missing_panics_doc = "warn"`.

- [ ] **Step 3: Add the D-R4 attribute to `pub mod proto` in `crates/vegapunk-client/src/lib.rs`**
- [ ] **Step 4: Run `cargo clippy --workspace --all-targets --locked -- -D warnings`; expect exit 0. Then `./scripts/test-all.sh`; expect ALL GREEN**
- [ ] **Step 5: Commit as Run D5, one PR**

## Completion conditions

- `[workspace.lints]` holds `missing_docs`, `clippy::missing_errors_doc` and `clippy::missing_panics_doc`, and `cargo clippy --workspace --all-targets --locked -- -D warnings` passes.
- `crates/core/tests/no_comments.rs` allows the rustdoc lines of SC-11 and still fails `//!`.
- Every crate in `CHECKED_CRATES` has the rustdoc that `missing_docs` reports and a `#![doc = "..."]` crate root.
- `./scripts/test-all.sh` is ALL GREEN.
- Each run passes the review gate of `~/.claude/specs/orchestration.md` §7.
