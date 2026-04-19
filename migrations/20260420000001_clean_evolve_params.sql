-- Remove stale sl_pct / allocation_pct from strategy_params and
-- strategies.default_params that are no longer used by the evolve
-- strategy (ATR-based SL + risk-linked sizing replaced them).
BEGIN;

UPDATE strategy_params
SET params = params - 'sl_pct' - 'allocation_pct'
WHERE strategy_name = 'donchian_trend_evolve_v1';

UPDATE strategies
SET default_params = default_params - 'sl_pct' - 'allocation_pct'
WHERE name = 'donchian_trend_evolve_v1';

COMMIT;
