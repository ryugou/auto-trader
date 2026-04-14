-- Live trading support schema additions.
-- - Extend trade_status with 'pending' (order sent, waiting on fill)
--   and 'inconsistent' (DB <-> exchange divergence, manual fix needed).
-- - Add bitFlyer child order identifiers on trades.
-- - Partial unique index: at most one active (pending+open) trade per
--   (account, strategy, pair) -- prevents duplicate entries after restart.
-- - risk_halts table: persists Kill Switch activations so restarts
--   re-apply existing halts.

-- Pattern B: status is TEXT without CHECK constraint (confirmed in Step 1).
-- No existing CHECK constraint to drop, so we add a new one.
ALTER TABLE trades
    ADD CONSTRAINT trades_status_check
    CHECK (status IN ('pending', 'open', 'closed', 'inconsistent'));

ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS child_order_acceptance_id TEXT,
    ADD COLUMN IF NOT EXISTS child_order_id TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS trades_one_active_per_strategy_pair
    ON trades (paper_account_id, strategy_name, pair)
    WHERE status IN ('pending', 'open');

CREATE TABLE IF NOT EXISTS risk_halts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    paper_account_id UUID NOT NULL REFERENCES paper_accounts(id),
    reason TEXT NOT NULL,
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    halted_until TIMESTAMPTZ NOT NULL,
    released_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS risk_halts_account_active
    ON risk_halts (paper_account_id, halted_until)
    WHERE released_at IS NULL;
