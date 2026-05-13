#!/usr/bin/env bash
# scripts/test-all.sh
#
# 実装変更を含む commit 前に必ず通すべき全テスト・全 lint。
# 中途半端な実行を排除するため、CLAUDE.md からはこのスクリプトを単一の入口として参照する。
#
# 失敗箇所で即停止する。再現したい場合はエラー直前のコマンドを単体で打つ。
#
# 使い方:
#   ./scripts/test-all.sh
#
# 前提:
#   - protoc が PATH 上にある (macOS: brew install protobuf)
#   - DATABASE_URL が未設定の場合、ローカル docker-compose の Postgres
#     (localhost:15432) を仮定し docker compose で起動する

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Hook 経由など PATH が貧弱な環境でも cargo / protoc を見つけられるよう、
# 標準的なインストール先を補強する。シェルから直接動かす場合は既に
# 入っているはずなので冪等。
for p in "$HOME/.cargo/bin" /opt/homebrew/bin /usr/local/bin; do
  if [[ -d "$p" && ":$PATH:" != *":$p:"* ]]; then
    PATH="$p:$PATH"
  fi
done
export PATH

step() {
  printf '\n\033[1;34m== %s ==\033[0m\n' "$*"
}

ok() {
  printf '\033[1;32m[OK] %s\033[0m\n' "$*"
}

# ── 前提チェック ──────────────────────────────────────────────────────
step "preflight"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not on PATH (looked in \$HOME/.cargo/bin, /opt/homebrew/bin, /usr/local/bin)." >&2
  echo "Install via rustup." >&2
  exit 1
fi

if ! command -v protoc >/dev/null 2>&1; then
  echo "protoc not on PATH. Install: brew install protobuf" >&2
  exit 1
fi

# docker / docker compose の存在は preflight では強制しない。
# 後段の probe_db が成功すれば DB が既に外部で起動済とみなして compose は不要。
# probe_db が失敗した時のみ docker compose に頼るので、その段階でチェックする。

# ── DB 起動 (docker compose) ──────────────────────────────────────────
if [[ -z "${DATABASE_URL:-}" ]]; then
  export DATABASE_URL='postgresql://auto-trader:auto-trader@localhost:15432/auto_trader'
fi
# 認証情報を含めずに表示 (`postgresql://USER:PASS@HOST:PORT/DB` →
# `postgresql://***@HOST:PORT/DB` に置換)
echo "DATABASE_URL=$(printf '%s' "$DATABASE_URL" | sed -E 's|://[^@]+@|://***@|')"

# DB connectivity probe (worktree から docker compose を打つと既存 main 側
# コンテナと port 衝突するので、まず外から到達できるかを試す)。
#
# 優先順:
#   1. psql で実 SQL を打って確認 (一番厳密)
#   2. pg_isready で readyz だけ確認
#   3. bash の /dev/tcp 経由で TCP 到達だけ確認 (postgres プロトコルは未検査だが
#      "ポートに何かがいる" は確認できるので開発時の起動チェックには十分)
probe_db() {
  if command -v psql >/dev/null 2>&1; then
    PGCONNECT_TIMEOUT=2 psql "$DATABASE_URL" -c 'SELECT 1' >/dev/null 2>&1
  elif command -v pg_isready >/dev/null 2>&1; then
    pg_isready -d "$DATABASE_URL" -t 2 >/dev/null 2>&1
  else
    # scheme:// 以降の authority (= [user[:pass]@]host[:port]) を抽出してから
    # credentials と path を順に剥がす。credentials が無い URL でも安全。
    # port 省略時は PostgreSQL のデフォルト 5432 を使う。IPv6 (例: `[::1]`)
    # は本プロジェクトの DATABASE_URL では使わない前提で未対応。
    local authority host port
    authority=${DATABASE_URL#*://}
    authority=${authority%%/*}
    authority=${authority##*@}
    if [[ "$authority" == *:* ]]; then
      host=${authority%:*}
      port=${authority##*:}
    else
      host=$authority
      port=5432
    fi
    if [[ -z "$host" ]]; then
      return 1
    fi
    (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null
  fi
}

if probe_db; then
  ok "db reachable on $(echo "$DATABASE_URL" | sed 's/.*@//')"
elif [[ "${SKIP_DOCKER_COMPOSE:-0}" != "1" ]]; then
  if ! command -v docker >/dev/null 2>&1; then
    echo "ERROR: db not reachable and \`docker\` not on PATH — cannot start the DB automatically." >&2
    echo "Either start Postgres manually (and set DATABASE_URL) or install Docker." >&2
    exit 1
  fi
  if ! docker compose version >/dev/null 2>&1; then
    echo "ERROR: \`docker compose\` plugin missing — cannot start the DB automatically." >&2
    exit 1
  fi
  step "docker compose up -d db"
  docker compose up -d db >/dev/null
  # healthcheck 待ち (最大 30 秒)
  for _ in $(seq 1 30); do
    if probe_db; then
      ok "db is healthy"
      break
    fi
    sleep 1
  done
  if ! probe_db; then
    echo "db did not become reachable within 30s" >&2
    exit 1
  fi
else
  echo "db not reachable and SKIP_DOCKER_COMPOSE=1 — aborting" >&2
  exit 1
fi

# ── 静的チェック ──────────────────────────────────────────────────────
step "cargo fmt --all -- --check"
cargo fmt --all -- --check
ok "fmt clean"

step "cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings
ok "clippy clean"

# ── テスト本体 ────────────────────────────────────────────────────────
step "cargo test --workspace --lib --bins --tests"
# `--tests` で crates/*/tests/*.rs 直下の integration tests も拾う
# (--lib --bins だけだと crate-local integration tests が完全に skip される)。
# auto-trader-integration-tests crate もここで一括実行される
# (smoke / phase1-4 / mocks 含む)。
cargo test --workspace --lib --bins --tests
ok "lib + bin + per-crate integration tests passed (incl. auto-trader-integration-tests)"

step "cargo test --workspace --doc"
cargo test --workspace --doc
ok "doc tests passed"

# ── サマリ ────────────────────────────────────────────────────────────
printf '\n\033[1;32m================ ALL GREEN ================\033[0m\n'
