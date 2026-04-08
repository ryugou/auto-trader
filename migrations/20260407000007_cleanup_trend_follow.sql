-- Remove the legacy trend_follow_v1 FX strategy.
--
-- Decision: trend_follow_v1 is being retired alongside the earlier
-- crypto_trend_v1 cleanup (see 20260407000006). It's an FX-only
-- strategy that was never bound to a paper account in this repo, so
-- there is no trade history / daily_summary / paper_account data to
-- migrate. We just delete the catalog row.
--
-- The `paper_accounts.strategy → strategies(name) ON DELETE RESTRICT`
-- FK is satisfied because no paper_account references trend_follow_v1.

DELETE FROM strategies WHERE name = 'trend_follow_v1';
