-- paper_accounts テーブル（trades より先に作成 — FK 参照のため）
CREATE TABLE paper_accounts (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    exchange TEXT NOT NULL,
    initial_balance DECIMAL NOT NULL,
    current_balance DECIMAL NOT NULL,
    currency TEXT NOT NULL DEFAULT 'JPY',
    leverage DECIMAL NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- trades: exchange, quantity, leverage, fees, paper_account_id
ALTER TABLE trades ADD COLUMN exchange TEXT NOT NULL DEFAULT 'oanda';
ALTER TABLE trades ADD COLUMN quantity DECIMAL;
ALTER TABLE trades ADD COLUMN leverage DECIMAL NOT NULL DEFAULT 1;
ALTER TABLE trades ADD COLUMN fees DECIMAL NOT NULL DEFAULT 0;
ALTER TABLE trades ADD COLUMN paper_account_id UUID REFERENCES paper_accounts(id);
CREATE INDEX idx_trades_exchange ON trades (exchange);

-- price_candles: exchange + UNIQUE 制約更新
ALTER TABLE price_candles ADD COLUMN exchange TEXT NOT NULL DEFAULT 'oanda';
ALTER TABLE price_candles DROP CONSTRAINT price_candles_pair_timeframe_timestamp_key;
ALTER TABLE price_candles ADD CONSTRAINT price_candles_exchange_pair_tf_ts_key
    UNIQUE (exchange, pair, timeframe, timestamp);

-- daily_summary: exchange, paper_account_id
ALTER TABLE daily_summary ADD COLUMN exchange TEXT NOT NULL DEFAULT 'oanda';
ALTER TABLE daily_summary ADD COLUMN paper_account_id UUID REFERENCES paper_accounts(id);
ALTER TABLE daily_summary DROP CONSTRAINT daily_summary_date_strategy_name_pair_mode_key;
ALTER TABLE daily_summary ADD CONSTRAINT daily_summary_unique_key
    UNIQUE (date, strategy_name, pair, mode, exchange, paper_account_id);
CREATE UNIQUE INDEX daily_summary_fx_unique
    ON daily_summary (date, strategy_name, pair, mode, exchange)
    WHERE paper_account_id IS NULL;
