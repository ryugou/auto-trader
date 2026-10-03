-- シミュレーション基盤 (crates/sim, auto-trader-sim) 用のテーブル。
-- 既存のテーブル (price_candles 含む) は一切変更しない。
--
-- 正本: docs/superpowers/specs/2026-10-01-simulation-foundation-design.md
--   5.1 章 (sim_candles) / 12 章 (sim_scripts, sim_batches, sim_runs)
--
-- 注意: 本番 DB では auto-trader (売買プロセス) の起動時にだけ適用する。
-- auto-trader-sim migrate を本番 DB に対して実行してはならない (spec 16 章)。

CREATE TABLE sim_candles (
    exchange   TEXT        NOT NULL,
    pair       TEXT        NOT NULL,
    timeframe  TEXT        NOT NULL,
    open_time  TIMESTAMPTZ NOT NULL,
    bid_open   NUMERIC(10,3) NOT NULL,
    bid_high   NUMERIC(10,3) NOT NULL,
    bid_low    NUMERIC(10,3) NOT NULL,
    bid_close  NUMERIC(10,3) NOT NULL,
    ask_open   NUMERIC(10,3) NOT NULL,
    ask_high   NUMERIC(10,3) NOT NULL,
    ask_low    NUMERIC(10,3) NOT NULL,
    ask_close  NUMERIC(10,3) NOT NULL,
    PRIMARY KEY (exchange, pair, timeframe, open_time)
);

CREATE TABLE sim_scripts (
    id            UUID PRIMARY KEY,
    name          TEXT NOT NULL,
    source        TEXT NOT NULL,
    source_sha256 TEXT NOT NULL UNIQUE,
    parent_id     UUID REFERENCES sim_scripts(id),
    origin        TEXT NOT NULL CHECK (origin IN ('human', 'llm')),
    note          TEXT NOT NULL DEFAULT '',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sim_batches (
    id          UUID PRIMARY KEY,
    script_id   UUID NOT NULL REFERENCES sim_scripts(id),
    status      TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    period_from TIMESTAMPTZ NOT NULL,
    period_to   TIMESTAMPTZ NOT NULL,
    bar_count   INTEGER NOT NULL,
    total_runs  INTEGER NOT NULL,
    config      JSONB NOT NULL,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);

CREATE TABLE sim_runs (
    id                    UUID PRIMARY KEY,
    batch_id              UUID NOT NULL REFERENCES sim_batches(id),
    params                JSONB NOT NULL,
    status                TEXT NOT NULL CHECK (status IN ('ok', 'script_error')),
    error                 TEXT,
    total_pips            DOUBLE PRECISION,
    trade_count           INTEGER,
    win_rate              DOUBLE PRECISION,
    max_drawdown_pips     DOUBLE PRECISION,
    time_in_market        DOUBLE PRECISION,
    protective_stop_count INTEGER,
    metrics               JSONB,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (batch_id, params)
);

CREATE INDEX sim_batches_script_id_idx ON sim_batches (script_id);
