-- Retroactively lock margin for currently open paper trades.
--
-- Before this change, `paper_accounts.current_balance` represented
-- "cash + locked margin" (margin was implicit). After this change the
-- column means "free cash" — `execute_with_quantity` deducts margin
-- on open and `close_position` refunds it on close. To bring existing
-- open positions in line with the new accounting we deduct their
-- margin from the corresponding accounts and emit a backfill
-- `margin_lock` event so balance-history reconstruction stays
-- correct.
--
-- Only crypto-style trades (with a non-NULL `quantity`) participate.
-- FX paper trades stored quantity as NULL and never had a notional
-- margin tied to the cash balance, so they are skipped here.
BEGIN;

INSERT INTO paper_account_events (paper_account_id, event_type, amount, occurred_at, reference_id)
SELECT
    t.paper_account_id,
    'margin_lock',
    -(t.quantity * t.entry_price / t.leverage),
    t.entry_at,
    t.id
FROM trades t
WHERE t.status = 'open'
  AND t.paper_account_id IS NOT NULL
  AND t.quantity IS NOT NULL
  AND t.leverage > 0;

UPDATE paper_accounts pa
   SET current_balance = current_balance - (
        SELECT COALESCE(SUM(t.quantity * t.entry_price / t.leverage), 0)
          FROM trades t
         WHERE t.paper_account_id = pa.id
           AND t.status = 'open'
           AND t.quantity IS NOT NULL
           AND t.leverage > 0
    ),
    updated_at = NOW()
 WHERE EXISTS (
    SELECT 1 FROM trades t
     WHERE t.paper_account_id = pa.id
       AND t.status = 'open'
       AND t.quantity IS NOT NULL
       AND t.leverage > 0
 );

COMMIT;
