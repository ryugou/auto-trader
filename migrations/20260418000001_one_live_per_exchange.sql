-- 同一 exchange に live 口座は 1 件のみ許可。
-- bitFlyer API client がシングルトンで、複数 live 行があると margin/collateral 共有で会計破綻する。
-- 大小文字 / 空白のバリエーションで制約をすり抜けないよう、正規化を DB レベルで強制。
BEGIN;

-- 既存行を正規化。
UPDATE trading_accounts
SET exchange = lower(btrim(exchange, E' \t\n\r'))
WHERE exchange <> lower(btrim(exchange, E' \t\n\r'));

-- DB CHECK: exchange は lowercase alphanumeric + underscore のみ許可。
-- trim/lower 正規化の結果に加え、tab/newline 等の非可視文字も一括拒否。
ALTER TABLE trading_accounts
    ADD CONSTRAINT trading_accounts_exchange_normalized
    CHECK (exchange ~ '^[a-z0-9_]+$');

-- 正規化済み値での partial unique: exchange 毎に live は 1 件のみ。
CREATE UNIQUE INDEX IF NOT EXISTS trading_accounts_one_live_per_exchange
    ON trading_accounts (exchange)
    WHERE account_type = 'live';

COMMIT;
