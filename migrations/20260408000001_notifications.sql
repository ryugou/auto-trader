-- In-app notification log for trade open / close events.
-- Unread notifications are kept forever; read notifications are
-- purged after 30 days by the daily batch. Display fields are
-- denormalized (copied from trades + paper_accounts at write time)
-- so the dashboard dropdown can render without a JOIN.
CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL CHECK (kind IN ('trade_opened', 'trade_closed')),
    trade_id UUID NOT NULL REFERENCES trades(id) ON DELETE CASCADE,
    paper_account_id UUID NOT NULL,
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    direction TEXT NOT NULL,
    price NUMERIC NOT NULL,
    pnl_amount NUMERIC,
    exit_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    read_at TIMESTAMPTZ,
    -- A `trade_closed` notification must carry pnl_amount + exit_reason;
    -- a `trade_opened` notification must leave them NULL. Enforce at
    -- schema level so callers can't accidentally mix the two shapes.
    CONSTRAINT notifications_kind_fields CHECK (
        (kind = 'trade_opened' AND pnl_amount IS NULL AND exit_reason IS NULL)
        OR
        (kind = 'trade_closed' AND pnl_amount IS NOT NULL AND exit_reason IS NOT NULL)
    )
);

CREATE INDEX idx_notifications_created_at ON notifications (created_at DESC);
CREATE INDEX idx_notifications_unread ON notifications (read_at) WHERE read_at IS NULL;
