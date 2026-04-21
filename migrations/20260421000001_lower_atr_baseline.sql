-- Lower atr_baseline_bars default from 50 → 20 for H1 timeframe.
-- With 1H candles, 50 bars requires 65+ bars before signal fires (≈ 3 days
-- of live data). 20 bars requires 35 bars (≈ 1.5 days) and 20 hours of ATR
-- baseline is still sufficient for volatility filtering.
BEGIN;

UPDATE strategy_params
SET params = jsonb_set(params, '{atr_baseline_bars}', '20')
WHERE strategy_name = 'donchian_trend_evolve_v1'
  AND (params->>'atr_baseline_bars')::int = 50;

UPDATE strategies
SET default_params = jsonb_set(default_params, '{atr_baseline_bars}', '20')
WHERE name = 'donchian_trend_evolve_v1'
  AND default_params->>'atr_baseline_bars' = '50';

COMMIT;
