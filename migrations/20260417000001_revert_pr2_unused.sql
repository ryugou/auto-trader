BEGIN;

-- Drop Kill Switch infrastructure (halts were never used in practice).
DROP INDEX IF EXISTS risk_halts_account_active;
DROP INDEX IF EXISTS risk_halts_one_active_per_account;
DROP TABLE IF EXISTS risk_halts CASCADE;

-- Drop duplicate-position partial unique index (RiskGate belt-and-suspenders).
DROP INDEX IF EXISTS trades_one_active_per_strategy_pair;

COMMIT;
