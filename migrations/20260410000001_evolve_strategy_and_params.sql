-- strategy_params: runtime-mutable parameters for evolve strategies.
-- Reads at startup + weekly batch update. Existing const-based
-- strategies never touch this table.
CREATE TABLE strategy_params (
    strategy_name TEXT PRIMARY KEY REFERENCES strategies(name),
    params JSONB NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- New columns on trades for enriched ingest + feedback tracking.
ALTER TABLE trades ADD COLUMN IF NOT EXISTS entry_indicators JSONB;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS vegapunk_search_id TEXT;

-- strategies catalog entry for the evolve variant.
INSERT INTO strategies (name, display_name, category, risk_level, description, algorithm, default_params)
VALUES ('donchian_trend_evolve_v1', 'ブレイクアウト進化版 (Donchian Evolve)',
        'crypto', 'medium',
        'donchian_trend_v1 ベース。Vegapunk 学習ループでパラメータを週次自動更新。baseline (normal) との比較検証用。',
        '(donchian_trend_v1 と同一アルゴリズム。パラメータのみ可変。)',
        '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb)
ON CONFLICT (name) DO NOTHING;

-- Initial params for evolve (identical to donchian_trend_v1 baseline).
INSERT INTO strategy_params (strategy_name, params)
VALUES ('donchian_trend_evolve_v1',
        '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb)
ON CONFLICT (strategy_name) DO NOTHING;

-- Evolve paper account.
INSERT INTO paper_accounts (id, name, exchange, initial_balance, current_balance,
                            currency, leverage, strategy, account_type, created_at, updated_at)
VALUES ('a0000000-0000-0000-0000-000000000020', 'crypto_evolve_v1',
        'bitflyer_cfd', 30000, 30000, 'JPY', 2,
        'donchian_trend_evolve_v1', 'paper', NOW(), NOW())
ON CONFLICT (id) DO NOTHING;

-- System notifications for automated batch events (weekly param updates etc).
-- Separate from the trade-specific notifications table which requires
-- trade_id/paper_account_id foreign keys.
CREATE TABLE system_notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    message TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    read_at TIMESTAMPTZ
);

CREATE INDEX idx_system_notifications_created_at ON system_notifications (created_at DESC);
