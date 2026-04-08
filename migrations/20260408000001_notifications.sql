-- In-app notification log for trade open / close events.
-- Unread notifications are kept forever; read notifications are
-- purged after 30 days by the daily batch. Display fields are
-- denormalized (copied from trades + paper_accounts at write time)
-- so the dashboard dropdown can render without a JOIN.
--
-- The `gen_random_uuid()` default relies on Postgres 13+ where it
-- is part of core (no extension required). The project pins
-- Postgres 16 via docker-compose.yml, so this is safe without a
-- `CREATE EXTENSION pgcrypto`. Adding that line would pull a
-- runtime permission dependency in for no benefit on a supported
-- Postgres version.
CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL CHECK (kind IN ('trade_opened', 'trade_closed')),
    trade_id UUID NOT NULL REFERENCES trades(id) ON DELETE CASCADE,
    paper_account_id UUID NOT NULL,
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('long', 'short')),
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
-- Two complementary partial indexes on `read_at`:
--  - the unread one supports the bell badge `WHERE read_at IS NULL`
--    count and the `unread_only=true` list filter
--  - the read one supports the daily `purge_old_read` query, which
--    runs `WHERE read_at IS NOT NULL AND read_at < NOW() - INTERVAL
--    '30 days'`. Without this index the purge would full-scan the
--    table once a day and the cost grows linearly with history.
CREATE INDEX idx_notifications_unread ON notifications (read_at) WHERE read_at IS NULL;
CREATE INDEX idx_notifications_read_at ON notifications (read_at) WHERE read_at IS NOT NULL;
