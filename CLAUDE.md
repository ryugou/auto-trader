# auto-trader プロジェクトルール

このファイルはプロジェクト固有のルール。グローバルルール (`~/.claude/CLAUDE.md`) は別途常時読み込まれる。

## 【厳守】commit 前のテスト実行

**実装変更を含む commit 前には**、以下のシミュレーションテスト群を必ず実行し、全 pass を確認すること。test/lint/type-check の出力確認は `superpowers:verification-before-completion` の必須項目に加えて、これも必須。

```bash
docker compose up -d db
DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader' \
  cargo test -p auto-trader-integration-tests
```

このコマンドは以下を全て含む:

- `smoke_test.rs::full_integration_smoke_test` — DB アカウント seed + CSV から price candle 投入 + 全 mock サーバ (GMO/Slack/Gemini/Vegapunk/BitflyerWs) 動作確認
- `phase3_execution.rs` / `phase3_execution_flow.rs` / `phase3_close_flow.rs` — 実機相当の `trader.execute(&signal)` フロー (signal 生成 → trade 成立 → close)
- `phase3_bb_mean_revert.rs` / `phase3_donchian_trend.rs` / `phase3_donchian_evolve.rs` / `phase3_squeeze_momentum.rs` / `phase3_swing_llm.rs` — 各戦略の price candle 投入 → on_price → signal 生成 までの end-to-end
- `phase1_*` / `phase2_*` / `phase3_*` の startup・API・トレード関連 全件

これらが green であることが「**理論上システムが正常にトレードできることを保証する**」最低条件。`cargo test -p auto-trader-core` や `cargo test -p auto-trader-executor` だけでは保証にならない。

ドキュメントだけの変更 (typo・コメント追加等) はこの限りでない。
