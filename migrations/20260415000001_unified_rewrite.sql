-- Unified trader rewrite — wipe all stateful tables and rebuild with clean schema.
-- 注意: 既存の paper trade データは全消失する。deploy 前の最終段階で
-- 実行される想定 (本 migration 適用 = 旧データ破棄の合意と同値)。

BEGIN;

-- 1) drop old tables and types
DROP TABLE IF EXISTS paper_account_events CASCADE;
DROP TABLE IF EXISTS trades CASCADE;
DROP TABLE IF EXISTS paper_accounts CASCADE;
DROP TABLE IF EXISTS notifications CASCADE;
DROP TABLE IF EXISTS strategy_params CASCADE;
DROP TABLE IF EXISTS strategies CASCADE;
DROP TABLE IF EXISTS risk_halts CASCADE;  -- PR #38 で作ったが未使用
-- 旧 CHECK 制約 / partial unique index は TABLE drop で一緒に消える

-- 2) strategies (戦略メタ)
CREATE TABLE IF NOT EXISTS strategies (
    name TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    category TEXT NOT NULL,
    risk_level TEXT NOT NULL,
    description TEXT,
    algorithm TEXT,
    default_params JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 3) strategy_params (戦略別パラメータ、vegapunk 学習ループ向け)
CREATE TABLE IF NOT EXISTS strategy_params (
    strategy_name TEXT PRIMARY KEY REFERENCES strategies(name),
    params JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 4) trading_accounts (paper_accounts 置換)
CREATE TABLE IF NOT EXISTS trading_accounts (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    account_type TEXT NOT NULL CHECK (account_type IN ('paper', 'live')),
    exchange TEXT NOT NULL,
    strategy TEXT NOT NULL REFERENCES strategies(name),
    initial_balance NUMERIC NOT NULL CHECK (initial_balance >= 0),
    current_balance NUMERIC NOT NULL,
    leverage NUMERIC NOT NULL CHECK (leverage >= 1),
    currency TEXT NOT NULL DEFAULT 'JPY',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 5) trades (clean)
CREATE TABLE IF NOT EXISTS trades (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES trading_accounts(id) ON DELETE RESTRICT,
    strategy_name TEXT NOT NULL REFERENCES strategies(name),
    pair TEXT NOT NULL,
    exchange TEXT NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('long', 'short')),
    entry_price NUMERIC NOT NULL,
    exit_price NUMERIC,
    quantity NUMERIC NOT NULL CHECK (quantity > 0),
    leverage NUMERIC NOT NULL,
    fees NUMERIC NOT NULL DEFAULT 0,
    stop_loss NUMERIC NOT NULL,
    take_profit NUMERIC,
    entry_at TIMESTAMPTZ NOT NULL,
    exit_at TIMESTAMPTZ,
    pnl_amount NUMERIC,
    exit_reason TEXT,
    status TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    max_hold_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS trades_account_status ON trades (account_id, status);
CREATE INDEX IF NOT EXISTS trades_account_entry_at ON trades (account_id, entry_at DESC);

-- 6) paper_account_events (残高履歴、カラム名追従)
CREATE TABLE IF NOT EXISTS account_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id UUID NOT NULL REFERENCES trading_accounts(id) ON DELETE RESTRICT,
    trade_id UUID REFERENCES trades(id),
    event_type TEXT NOT NULL CHECK (event_type IN ('margin_lock', 'margin_release', 'trade_open', 'trade_close', 'overnight_fee', 'balance_sync')),
    amount NUMERIC NOT NULL,
    balance_after NUMERIC NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    metadata JSONB
);
CREATE INDEX IF NOT EXISTS account_events_account_time ON account_events (account_id, occurred_at DESC);

-- 7) notifications (UI ベル、クリーン再作成)
CREATE TABLE IF NOT EXISTS notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL,
    account_id UUID REFERENCES trading_accounts(id),
    trade_id UUID REFERENCES trades(id),
    strategy_name TEXT,
    pair TEXT,
    direction TEXT,
    price NUMERIC,
    pnl_amount NUMERIC,
    exit_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    read_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS notifications_unread ON notifications (created_at DESC) WHERE read_at IS NULL;

-- 8) strategies seed (旧 migration 20260407000005 / 20260410000001 の内容と等価)
INSERT INTO strategies (name, display_name, category, risk_level, description, algorithm, default_params) VALUES
  ('bb_mean_revert_v1', '慎重 (low risk)', 'crypto', 'low',
   'ボリンジャーバンド下抜け/上抜け後の平均回帰を狙う。BB ± 2.5σ / RSI 14 / ATR 14。24h タイムリミット。',
   'Bollinger Bands + RSI mean reversion',
   '{"bb_period":20,"bb_stddev":2.5,"rsi_period":14,"atr_period":14,"sl_max_pct":0.02,"time_limit_hours":24}'::jsonb),
  ('donchian_trend_v1', '標準ブレイクアウト v1 (Donchian)', 'crypto', 'medium',
   '20 本ブレイクアウト + 10 本トレーリング。ATR で SL 固定。Turtle System 系。',
   'Donchian channel breakout with trailing exit',
   '{"entry_channel":20,"exit_channel":10,"atr_period":14,"atr_baseline_bars":50}'::jsonb),
  ('squeeze_momentum_v1', '攻め (high risk)', 'crypto', 'high',
   'BB squeeze (KC 内収束) + ブレイクアウト + EMA トレーリング。48h タイムリミット。',
   'Squeeze momentum + EMA trailing',
   '{"bb_period":20,"kc_period":20,"atr_period":14,"ema_trail_period":21,"squeeze_bars":6}'::jsonb),
  ('donchian_trend_evolve_v1', 'ブレイクアウト進化版 (Donchian Evolve)', 'crypto', 'medium',
   'donchian_trend_v1 ベース。Vegapunk 学習ループでパラメータを週次自動更新。baseline (通常) との A/B.',
   '(donchian_trend_v1 と同一アルゴリズム。パラメータのみ可変。)',
   '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb)
ON CONFLICT (name) DO NOTHING;

-- 9) strategy_params seed (evolve 用、initial は baseline と同じ)
INSERT INTO strategy_params (strategy_name, params) VALUES
  ('donchian_trend_evolve_v1',
   '{"entry_channel":20,"exit_channel":10,"sl_pct":0.03,"allocation_pct":1.0,"atr_baseline_bars":50}'::jsonb)
ON CONFLICT (strategy_name) DO NOTHING;

-- 10) daily_summary: paper_account_id → account_id (trading_accounts FK)
--     The CASCADE on paper_accounts drop already removed the FK constraint,
--     but the column is still named paper_account_id. Rename + re-add FK.
ALTER TABLE daily_summary RENAME COLUMN paper_account_id TO account_id;
-- Re-create unique constraints with account_id semantics.
-- The old constraints were dropped by the paper_accounts CASCADE, but
-- the unique index daily_summary_fx_unique may still exist.
DROP INDEX IF EXISTS daily_summary_fx_unique;
ALTER TABLE daily_summary DROP CONSTRAINT IF EXISTS daily_summary_unique_key;
ALTER TABLE daily_summary ADD CONSTRAINT daily_summary_unique_key
    UNIQUE (date, strategy_name, pair, mode, exchange, account_id);
CREATE UNIQUE INDEX IF NOT EXISTS daily_summary_no_account_unique
    ON daily_summary (date, strategy_name, pair, mode, exchange)
    WHERE account_id IS NULL;
ALTER TABLE daily_summary
    ADD CONSTRAINT daily_summary_account_id_fk
    FOREIGN KEY (account_id) REFERENCES trading_accounts(id);

-- 11) trading_accounts seed (paper 4 アカウント)
INSERT INTO trading_accounts (id, name, account_type, exchange, strategy, initial_balance, current_balance, leverage, currency) VALUES
  ('a0000000-0000-0000-0000-000000000010', '安全', 'paper', 'bitflyer_cfd', 'bb_mean_revert_v1', 30000, 30000, 2, 'JPY'),
  ('a0000000-0000-0000-0000-000000000011', '通常', 'paper', 'bitflyer_cfd', 'donchian_trend_v1', 30000, 30000, 2, 'JPY'),
  ('a0000000-0000-0000-0000-000000000012', '攻め', 'paper', 'bitflyer_cfd', 'squeeze_momentum_v1', 30000, 30000, 2, 'JPY'),
  ('a0000000-0000-0000-0000-000000000020', 'vegapunk連動', 'paper', 'bitflyer_cfd', 'donchian_trend_evolve_v1', 30000, 30000, 2, 'JPY')
ON CONFLICT (id) DO NOTHING;

COMMIT;
