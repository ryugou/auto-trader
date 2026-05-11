# auto-trader プロジェクトルール

このファイルはプロジェクト固有のルール。グローバルルール (`~/.claude/CLAUDE.md`) は別途常時読み込まれる。

## 【厳守】commit 前のテスト実行

**実装変更を含む commit 前には必ず `./scripts/test-all.sh` を実行し、最後に `ALL GREEN` が出ることを確認する。**

このスクリプトが単一の入口。**個別コマンドを部分的に打つ運用は禁止** (取りこぼし防止)。スクリプトは以下を順に実行し、いずれか 1 段階でも失敗したら即停止する:

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace --lib --bins` (全 crate の unit + bin tests)
4. `cargo test -p auto-trader-integration-tests` (smoke / phase1-4 / mocks 全件)
5. `cargo test --workspace --doc` (全 crate の doc tests)

`auto-trader-integration-tests` には以下が含まれ、これが green であることが「**理論上システムが正常にトレードできることを保証する**」最低条件:

- `smoke_test.rs::full_integration_smoke_test` — DB アカウント seed + CSV から price candle 投入 + 全 mock サーバ (GMO/Slack/Gemini/Vegapunk/BitflyerWs) 動作確認
- `phase3_execution.rs` / `phase3_execution_flow.rs` / `phase3_close_flow.rs` — 実機相当の `trader.execute(&signal)` フロー (signal 生成 → trade 成立 → close)
- `phase3_bb_mean_revert.rs` / `phase3_donchian_trend.rs` / `phase3_donchian_evolve.rs` / `phase3_squeeze_momentum.rs` / `phase3_swing_llm.rs` — 各戦略の price candle 投入 → on_price → signal 生成 までの end-to-end
- `phase1_*` / `phase2_*` / `phase3_*` の startup・API・トレード関連 全件
- `phase4_external.rs` — 外部 API (GMO 本番 / OANDA practice / Vegapunk) 接続。auth token 未設定の項目はテスト内で SKIPPED ログを出して pass

ドキュメントだけの変更 (typo・コメント追加等) はこの限りでない。

## 前提

`./scripts/test-all.sh` は以下を前提とする:

- `protoc` が PATH 上 (macOS: `brew install protobuf`)
- `DATABASE_URL` 未設定なら localhost:15432 の docker-compose Postgres を仮定し、必要なら自動起動する
- 外部 API 接続を試したい場合は `VEGAPUNK_AUTH_TOKEN` / `OANDA_API_KEY` / `OANDA_ACCOUNT_ID` 等を env で渡す (未設定なら該当テストは SKIPPED)

## 【強制】git hook セットアップ

Clone 後に 1 回実行:

```bash
./scripts/install-hooks.sh
```

これにより `core.hooksPath` が `.githooks/` に切り替わり、以下が hook で強制される:

- `.githooks/pre-push`:
  1. `main` / `master` への直 push をドキュメント以外で拒否
  2. **ドキュメント以外の変更を含む push は `scripts/test-all.sh` が pass しなければ拒否**

非常時のみの override: `git push --no-verify` (本来禁止。CI でも検出されるので使わない)。
