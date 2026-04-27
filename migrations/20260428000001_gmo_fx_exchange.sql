-- Migrate the FX paper account from 'oanda' to 'gmo_fx'.
--
-- GMO Coin FX replaces OANDA as the price-data source for FX paper trading.
-- The GMO Coin FX Public REST API requires no authentication, so the feed
-- works immediately without configuring OANDA credentials.
--
-- Preflight guard: refuse to migrate if any open/closing trades exist on
-- this account to prevent leaving orphaned positions with a stale exchange tag.
BEGIN;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM trades
        WHERE account_id = 'a0000000-0000-0000-0000-000000000030'
          AND status IN ('open', 'closing')
    ) THEN
        RAISE EXCEPTION 'Cannot migrate FX account to gmo_fx: open trades exist';
    END IF;
END;
$$;

UPDATE trading_accounts
SET exchange = 'gmo_fx'
WHERE id = 'a0000000-0000-0000-0000-000000000030';

COMMIT;
