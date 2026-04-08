-- Truncate all yen-denominated amounts in the ledger to whole yen.
--
-- After this change, yen is integer-only throughout the system:
--
--   PaperTrader::truncate_yen (Rust side)   — all new writes
--   dashboard.rs TRUNC(...)                 — all new reads
--   this migration                          — existing rows
--
-- Historical rows written before the truncation contract was introduced
-- may carry fractional yen (e.g. pnl 1234.567). Left alone, they would
-- show up in the dashboard alongside the new integer values and the
-- reconstructed balance history would drift by their sub-yen sum. We
-- normalize them here to match the new contract.
--
-- We use TRUNC (toward zero) to match RoundingStrategy::ToZero in the
-- Rust helper. Both positive and negative amounts (losing trades, fees,
-- margin_lock events with negative amount) truncate toward zero, so the
-- sign is preserved and |rounded| <= |original| in every case.
BEGIN;

UPDATE paper_accounts
   SET current_balance = TRUNC(current_balance),
       initial_balance = TRUNC(initial_balance),
       updated_at      = NOW()
 WHERE current_balance <> TRUNC(current_balance)
    OR initial_balance <> TRUNC(initial_balance);

UPDATE trades
   SET pnl_amount = TRUNC(pnl_amount)
 WHERE pnl_amount IS NOT NULL
   AND pnl_amount <> TRUNC(pnl_amount);

UPDATE trades
   SET fees = TRUNC(fees)
 WHERE fees <> TRUNC(fees);

UPDATE paper_account_events
   SET amount = TRUNC(amount)
 WHERE amount <> TRUNC(amount);

-- daily_summary aggregates feed `get_summary` and `get_pnl_history`
-- directly via SUM(total_pnl) / MAX(max_drawdown) without an outer
-- TRUNC. If we left fractional yen here, the dashboard "Total PnL"
-- and "Max DD" cards would still show non-integer yen even though
-- the underlying trades.pnl_amount is now integer. Truncate the
-- aggregate columns too. (The daily batch job will keep producing
-- integer yen going forward because trades.pnl_amount is integer.)
UPDATE daily_summary
   SET total_pnl    = TRUNC(total_pnl),
       max_drawdown = TRUNC(max_drawdown)
 WHERE total_pnl    <> TRUNC(total_pnl)
    OR max_drawdown <> TRUNC(max_drawdown);

COMMIT;
