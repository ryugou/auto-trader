# Testing

## Unit tests (no DB required)

```bash
cargo test -p auto-trader-core -p auto-trader-market -p auto-trader-strategy -p auto-trader-notify
```

## Integration tests (DB required)

```bash
docker compose up -d db
export DATABASE_URL="postgresql://auto-trader:auto-trader@localhost:15432/auto_trader"
cargo test --workspace
```

DB-dependent tests use `#[sqlx::test]` and panic with `DATABASE_URL must be set` without it.

Affected crates: `auto-trader` (startup_reconcile), `auto-trader-db`, `auto-trader-executor`.
