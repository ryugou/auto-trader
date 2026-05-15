-- Add exchange-side position identifier to trades for GMO FX /v1/closeOrder.
-- NULL is the explicit "not applicable" value (bitFlyer nets positions internally).

ALTER TABLE trades ADD COLUMN IF NOT EXISTS exchange_position_id TEXT;
COMMENT ON COLUMN trades.exchange_position_id IS
  'Exchange-side position identifier. Required by GMO FX /v1/closeOrder. NULL for exchanges that net positions implicitly (bitFlyer).';
