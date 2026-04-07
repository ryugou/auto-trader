-- Clean up the legacy crypto_trend_v1 experiment.
--
-- Decision: the original `crypto_real` (10,000 JPY) and `crypto_100k`
-- (~100,000 JPY) paper accounts that ran the SMA-cross
-- `crypto_trend_v1` strategy are being retired. Their balances /
-- positions are no longer useful for the new 3-strategy A/B test:
--
-- - crypto_real had a structural margin shortfall: even at 50 %
--   allocation it could not afford the 0.001 BTC minimum lot at
--   current BTC prices.
-- - crypto_100k was holding a single legacy SHORT position from
--   2026-04-06 03:40 UTC and could not take new entries on the same
--   pair.
-- - crypto_trend_v1 itself overlaps in spirit with the new
--   donchian_trend_v1 / squeeze_momentum_v1 implementations and is
--   no longer needed as a baseline.
--
-- This migration removes everything in the right order so the
-- `paper_accounts.strategy → strategies(name)` FK doesn't block.

BEGIN;

-- We resolve account IDs via stable attributes (name / strategy)
-- rather than hard-coded UUIDs. Earlier seed migrations used
-- `ON CONFLICT (name) DO UPDATE` without touching `id`, so an
-- environment that originally created these accounts via the REST
-- API could legitimately have different UUIDs. Filtering by name +
-- strategy makes the cleanup robust across all DB histories.

-- 1. Drop trade history for the two retired accounts. We do not need
--    to preserve these for analytics — the new 3 paper accounts are
--    the active experiment.
DELETE FROM trades
 WHERE paper_account_id IN (
    SELECT id
      FROM paper_accounts
     WHERE name IN ('crypto_real', 'crypto_100k')
        OR strategy = 'crypto_trend_v1'
 );

-- 2. Daily summary rows for those same accounts.
DELETE FROM daily_summary
 WHERE paper_account_id IN (
    SELECT id
      FROM paper_accounts
     WHERE name IN ('crypto_real', 'crypto_100k')
        OR strategy = 'crypto_trend_v1'
 );

-- 3. paper_account_events cascades on paper_accounts delete, so we
--    rely on that for cleanup. Delete the retired accounts directly
--    using the same stable attributes.
DELETE FROM paper_accounts
 WHERE name IN ('crypto_real', 'crypto_100k')
    OR strategy = 'crypto_trend_v1';

-- 4. Catalog row. With every referencing account gone, the FK
--    paper_accounts.strategy → strategies(name) is no longer holding
--    this row in place.
DELETE FROM strategies WHERE name = 'crypto_trend_v1';

COMMIT;
