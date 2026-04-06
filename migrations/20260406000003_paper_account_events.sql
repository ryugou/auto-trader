-- Event log of all balance changes for paper accounts.
-- Used to reconstruct daily balance history that includes overnight fees,
-- which previously could not be attributed to a specific date.
CREATE TABLE paper_account_events (
    id BIGSERIAL PRIMARY KEY,
    paper_account_id UUID NOT NULL REFERENCES paper_accounts(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,           -- 'trade_close' | 'overnight_fee'
    amount DECIMAL NOT NULL,            -- positive: balance up, negative: balance down
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reference_id UUID,                  -- e.g. trade_id
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_paper_account_events_account_time
    ON paper_account_events (paper_account_id, occurred_at);

-- Backfill closed trades so balance history before this migration is preserved.
-- Overnight fees cannot be backfilled (their accrual date is unknown), so any
-- previously deducted fees will simply be missing from the daily reconstruction.
INSERT INTO paper_account_events (paper_account_id, event_type, amount, occurred_at, reference_id)
SELECT
    paper_account_id,
    'trade_close',
    pnl_amount,
    exit_at,
    id
FROM trades
WHERE status = 'closed'
  AND paper_account_id IS NOT NULL
  AND pnl_amount IS NOT NULL
  AND exit_at IS NOT NULL;
