-- Live trading support schema additions.
-- - Extend trade_status with 'pending' (order sent, waiting on fill)
--   and 'inconsistent' (DB <-> exchange divergence, manual fix needed).
-- - Add bitFlyer child order identifiers on trades.
-- - Partial unique index: at most one active (pending+open) trade per
--   (account, strategy, pair) -- prevents duplicate entries after restart.
-- - risk_halts table: persists Kill Switch activations so restarts
--   re-apply existing halts.

-- Pattern B: status is TEXT without CHECK constraint (confirmed in Step 1).
-- DROP IF EXISTS keeps the migration idempotent for DR / manual re-runs;
-- ADD CONSTRAINT itself has no IF NOT EXISTS in PostgreSQL 16.
ALTER TABLE trades DROP CONSTRAINT IF EXISTS trades_status_check;
ALTER TABLE trades
    ADD CONSTRAINT trades_status_check
    CHECK (status IN ('pending', 'open', 'closed', 'inconsistent'));

-- Enforce mode/status consistency at the database level. The Rust side has
-- TradeStatus::assert_valid_for_mode() but it's debug_assert! only, so a
-- release build could still silently persist paper/backtest rows with
-- live-only statuses if a code path slipped past the guard. The CHECK
-- below is the durable safety net:
--   paper/backtest  → open | closed only
--   live            → any of the 4 statuses
ALTER TABLE trades DROP CONSTRAINT IF EXISTS trades_mode_status_consistency;
ALTER TABLE trades
    ADD CONSTRAINT trades_mode_status_consistency
    CHECK (
        mode = 'live'
        OR (mode IN ('paper', 'backtest') AND status IN ('open', 'closed'))
    );

ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS child_order_acceptance_id TEXT,
    ADD COLUMN IF NOT EXISTS child_order_id TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS trades_one_active_per_strategy_pair
    ON trades (paper_account_id, strategy_name, pair)
    WHERE status IN ('pending', 'open');

-- risk_halts is an audit log: halt history must survive account
-- deletion attempts. ON DELETE RESTRICT blocks paper_accounts
-- deletion if any halt row references it — forcing the operator
-- to consciously archive or reassign halts before removing an
-- account. CASCADE would silently wipe the audit trail.
CREATE TABLE IF NOT EXISTS risk_halts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    paper_account_id UUID NOT NULL REFERENCES paper_accounts(id) ON DELETE RESTRICT,
    reason TEXT NOT NULL,
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    halted_until TIMESTAMPTZ NOT NULL,
    released_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS risk_halts_account_active
    ON risk_halts (paper_account_id, halted_until)
    WHERE released_at IS NULL;
