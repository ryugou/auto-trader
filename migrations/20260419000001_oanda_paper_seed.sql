-- OANDA paper trading account (FX).
-- Uses the same strategies as bitFlyer paper accounts; FX parameters
-- are a follow-up tuning concern — existing strategies are indicator-
-- based and asset-agnostic.
BEGIN;

INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                               initial_balance, current_balance, leverage, currency) VALUES
  ('a0000000-0000-0000-0000-000000000030', 'FX 通常', 'paper', 'oanda',
   'donchian_trend_v1', 30000, 30000, 10, 'JPY')
ON CONFLICT (id) DO NOTHING;

COMMIT;
