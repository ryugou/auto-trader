# Testing

## Unit tests (no DB required)

```bash
cargo test -p auto-trader-core -p auto-trader-market -p auto-trader-strategy -p auto-trader-notify
```

These tests run without any external dependencies.

## Integration tests (DB required)

```bash
# Start the database
docker compose up -d db

# Set DATABASE_URL for sqlx::test
export DATABASE_URL="postgresql://auto-trader:auto-trader@localhost:15432/auto_trader"

# Run all tests including DB-dependent ones
cargo test --workspace
```

DB-dependent tests use `#[sqlx::test]` and require `DATABASE_URL` to be set.
Without it, these tests panic with `DATABASE_URL must be set: EnvVar(NotPresent)`.

Affected crates:
- `auto-trader` (app): `startup_reconcile` tests
- `auto-trader-db`: `trading_accounts`, `strategies` tests
- `auto-trader-executor`: `trader_test` integration tests
