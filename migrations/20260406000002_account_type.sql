-- Add account_type column to paper_accounts (paper | live).
-- Existing rows default to 'paper' (they are all paper accounts).
ALTER TABLE paper_accounts
    ADD COLUMN account_type TEXT NOT NULL DEFAULT 'paper';

-- Add account_type to daily_summary so Overview can be split per type.
-- Existing rows stay NULL and are ignored when filter is applied.
ALTER TABLE daily_summary
    ADD COLUMN account_type TEXT;
