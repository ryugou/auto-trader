-- Add an optional time-based fail-safe to trades.
--
-- Strategies that emit a position with a hard "give up after X hours"
-- contract (mean-reversion 24h, vol-breakout 48h, …) record that deadline
-- in `max_hold_until`. The position monitor force-closes the trade once
-- the wall clock passes that timestamp, even when neither SL/TP nor any
-- strategy-driven exit has fired. NULL means "no time limit".
ALTER TABLE trades ADD COLUMN max_hold_until TIMESTAMPTZ;
