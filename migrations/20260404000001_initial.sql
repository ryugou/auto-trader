CREATE TABLE trades (
    id UUID PRIMARY KEY,
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    direction TEXT NOT NULL,
    entry_price DECIMAL NOT NULL,
    exit_price DECIMAL,
    stop_loss DECIMAL NOT NULL,
    take_profit DECIMAL NOT NULL,
    entry_at TIMESTAMPTZ NOT NULL,
    exit_at TIMESTAMPTZ,
    pnl_pips DECIMAL,
    pnl_amount DECIMAL,
    exit_reason TEXT,
    mode TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_trades_strategy ON trades (strategy_name);
CREATE INDEX idx_trades_pair ON trades (pair);
CREATE INDEX idx_trades_mode ON trades (mode);
CREATE INDEX idx_trades_status ON trades (status);
CREATE INDEX idx_trades_entry_at ON trades (entry_at);

CREATE TABLE price_candles (
    id BIGSERIAL PRIMARY KEY,
    pair TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    open DECIMAL NOT NULL,
    high DECIMAL NOT NULL,
    low DECIMAL NOT NULL,
    close DECIMAL NOT NULL,
    volume INTEGER,
    timestamp TIMESTAMPTZ NOT NULL,
    UNIQUE (pair, timeframe, timestamp)
);

CREATE INDEX idx_candles_pair_tf ON price_candles (pair, timeframe);
CREATE INDEX idx_candles_timestamp ON price_candles (timestamp);

CREATE TABLE strategy_configs (
    id UUID PRIMARY KEY,
    strategy_name TEXT NOT NULL,
    version TEXT NOT NULL,
    params JSONB NOT NULL,
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE daily_summary (
    id BIGSERIAL PRIMARY KEY,
    date DATE NOT NULL,
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    mode TEXT NOT NULL,
    trade_count INTEGER NOT NULL DEFAULT 0,
    win_count INTEGER NOT NULL DEFAULT 0,
    total_pnl DECIMAL NOT NULL DEFAULT 0,
    max_drawdown DECIMAL NOT NULL DEFAULT 0,
    UNIQUE (date, strategy_name, pair, mode)
);

CREATE TABLE macro_events (
    id UUID PRIMARY KEY,
    summary TEXT NOT NULL,
    event_type TEXT NOT NULL,
    impact TEXT NOT NULL,
    event_at TIMESTAMPTZ NOT NULL,
    source TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_macro_events_type ON macro_events (event_type);
CREATE INDEX idx_macro_events_at ON macro_events (event_at);
