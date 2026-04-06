-- Add account_type column to paper_accounts (paper | live).
-- Existing rows default to 'paper' (they are all paper accounts).
ALTER TABLE paper_accounts
    ADD COLUMN account_type TEXT NOT NULL DEFAULT 'paper';

-- Add account_type to daily_summary so Overview can be split per type.
ALTER TABLE daily_summary
    ADD COLUMN account_type TEXT;

-- Backfill: copy account_type from paper_accounts when paper_account_id is set.
UPDATE daily_summary ds
SET account_type = pa.account_type
FROM paper_accounts pa
WHERE ds.paper_account_id = pa.id AND ds.account_type IS NULL;

-- Remaining rows (no paper_account_id) default to 'paper'.
UPDATE daily_summary SET account_type = 'paper' WHERE account_type IS NULL;
