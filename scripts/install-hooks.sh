#!/usr/bin/env bash
# scripts/install-hooks.sh
#
# Clone 後に 1 回実行する。`.githooks/` をリポジトリ共通の hook ディレクトリに
# 設定し、main 直 push 禁止の強制ルールを有効化する。テスト実行 (test-all.sh)
# は git hook では強制せず、Claude Code 経由なら PreToolUse hook、それ以外は
# 実装ワークフロー側の discipline で担保する (詳細は CLAUDE.md)。
#
# 使い方:
#   ./scripts/install-hooks.sh

set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

if [ ! -d .githooks ]; then
  echo "ERROR: .githooks/ directory not found at repo root." >&2
  exit 1
fi

# 既存の hook (例: .git/hooks/pre-push) と .githooks/pre-push の重複を避けるため
# core.hooksPath を切り替える。既存 .git/hooks/* は無効化される。
git config core.hooksPath .githooks
echo "core.hooksPath set to .githooks"

# 実行権限を担保 (Windows でも動くように)
chmod +x .githooks/* 2>/dev/null || true

echo ""
echo "Active hooks:"
ls -la .githooks/
