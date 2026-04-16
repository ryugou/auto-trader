-- live account は exchange 毎に 1 件のみ。bitFlyer API client が
-- シングルトンで 1 つの API key/secret を使う制約から、同一 exchange
-- に複数 live 行があると margin プール共有 / 清算リスク共有 / 会計
-- 乖離が発生する。DB レイヤで 2 件目の INSERT を拒否する。
BEGIN;
CREATE UNIQUE INDEX IF NOT EXISTS trading_accounts_one_live_per_exchange
    ON trading_accounts (exchange)
    WHERE account_type = 'live';
COMMIT;
