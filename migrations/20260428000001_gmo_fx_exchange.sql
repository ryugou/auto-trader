-- Migrate the FX paper account from 'oanda' to 'gmo_fx'.
--
-- GMO Coin FX replaces OANDA as the price-data source for FX paper trading.
-- The GMO Coin FX Public REST API requires no authentication, so the feed
-- works immediately without configuring OANDA credentials.
--
-- Existing trades on the 'oanda' exchange are unaffected (the account had
-- no open trades at the time of this migration).
BEGIN;

UPDATE trading_accounts
SET exchange = 'gmo_fx'
WHERE id = 'a0000000-0000-0000-0000-000000000030';

COMMIT;
