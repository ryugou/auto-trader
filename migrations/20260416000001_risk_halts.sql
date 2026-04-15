-- Kill Switch 発動記録。PR-1 の unified_rewrite で drop したテーブルを
-- RiskGate 実装に合わせて再作成。trading_accounts FK に合わせる。
BEGIN;

CREATE TABLE IF NOT EXISTS risk_halts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id UUID NOT NULL REFERENCES trading_accounts(id) ON DELETE RESTRICT,
    reason TEXT NOT NULL,
    daily_loss NUMERIC NOT NULL,
    loss_limit NUMERIC NOT NULL,
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    halted_until TIMESTAMPTZ NOT NULL,
    released_at TIMESTAMPTZ,
    CONSTRAINT risk_halts_halt_after_trigger
        CHECK (halted_until > triggered_at)
);

CREATE INDEX IF NOT EXISTS risk_halts_account_active
    ON risk_halts (account_id, triggered_at DESC)
    WHERE released_at IS NULL;

-- 同一 account の未解除 halt は 1 件のみ許可（並行 KillSwitch 二重発火防止）。
CREATE UNIQUE INDEX IF NOT EXISTS risk_halts_one_active_per_account
    ON risk_halts (account_id)
    WHERE released_at IS NULL;

-- 二重発注防止: 同一 account × strategy × pair で open/closing は1件まで。
-- RiskGate の pre-check と二重化。レースで潜り抜けた場合は DB が拒否する。
CREATE UNIQUE INDEX IF NOT EXISTS trades_one_active_per_strategy_pair
    ON trades (account_id, strategy_name, pair)
    WHERE status IN ('open', 'closing');

COMMIT;
