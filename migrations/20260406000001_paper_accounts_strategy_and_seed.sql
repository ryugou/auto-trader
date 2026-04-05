-- Add strategy column to paper_accounts.
-- DEFAULT '' is for DDL only — existing rows (if any) get empty strategy
-- and will be skipped at startup with a warning. Intended for fresh DBs.
ALTER TABLE paper_accounts ADD COLUMN IF NOT EXISTS strategy TEXT NOT NULL DEFAULT '';

-- Seed initial paper accounts
INSERT INTO paper_accounts (id, name, exchange, initial_balance, current_balance, currency, leverage, strategy)
VALUES
    ('a0000000-0000-0000-0000-000000000001', 'crypto_real', 'bitflyer_cfd', 5233, 5233, 'JPY', 2, 'crypto_trend_v1'),
    ('a0000000-0000-0000-0000-000000000002', 'crypto_100k', 'bitflyer_cfd', 100000, 100000, 'JPY', 2, 'crypto_trend_v1')
ON CONFLICT (name) DO UPDATE SET
    strategy = EXCLUDED.strategy,
    updated_at = NOW();
