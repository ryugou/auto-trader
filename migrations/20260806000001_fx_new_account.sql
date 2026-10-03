-- Retire the two unprofitable FX paper accounts without destroying their
-- trades/account_events, then start the replacement strategy from fresh cash.
BEGIN;

ALTER TABLE trading_accounts
    ADD COLUMN active BOOLEAN NOT NULL DEFAULT TRUE;

UPDATE trading_accounts
SET active = FALSE
WHERE id IN (
    'a0000000-0000-0000-0000-000000000031',
    'a0000000-0000-0000-0000-000000000032'
)
   OR (exchange = 'gmo_fx' AND name IN ('FX 安全', 'FX 攻め'));

INSERT INTO strategies (
    name, display_name, category, risk_level, description, algorithm, default_params
)
VALUES (
    'fx_new_v1', 'FX新 v1', 'fx', 'medium',
    'M15 Donchian breakout with H1 trend filter and 1% risk sizing.',
    'M5 candles are aggregated into M15. Entries use a 16-bar Donchian breakout, ATR expansion and the direction of H1 SMA(20)/SMA(50). Exits use an 8-bar reverse channel after 1R. Position size targets a 1% loss at the initial stop.',
    '{"entry_channel":16,"exit_channel":8,"atr_period":14,"target_risk_pct":0.01}'::jsonb
)
ON CONFLICT (name) DO NOTHING;

INSERT INTO trading_accounts (
    id, name, account_type, exchange, strategy,
    initial_balance, current_balance, leverage, currency, active
) VALUES (
    'a0000000-0000-0000-0000-000000000040', 'FX新', 'paper', 'gmo_fx',
    'fx_new_v1', 30000, 30000, 10, 'JPY', TRUE
)
ON CONFLICT (id) DO NOTHING;

COMMIT;
