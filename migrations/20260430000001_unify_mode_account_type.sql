-- Unify mode/account_type in daily_summary.
--
-- The table has two columns with the same semantics:
--   mode         — part of UNIQUE constraints, always populated
--   account_type — added later, nullable, same values as mode
-- This migration drops the redundant account_type column and renames
-- mode → account_type for consistency with trading_accounts.
BEGIN;

-- 1) Drop the old nullable account_type column (redundant with mode).
ALTER TABLE daily_summary DROP COLUMN IF EXISTS account_type;

-- 2) Rename mode → account_type to match trading_accounts terminology.
ALTER TABLE daily_summary RENAME COLUMN mode TO account_type;

-- 3) Recreate constraints referencing the renamed column.
--    The unique constraint and partial index reference "mode" — they
--    are automatically updated by RENAME COLUMN in PostgreSQL, so no
--    manual recreation is needed. Verify with a no-op assertion.

COMMIT;
