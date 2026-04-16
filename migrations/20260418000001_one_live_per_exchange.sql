-- 同一 exchange に live 口座は 1 件のみ許可。
-- bitFlyer API client がシングルトンで、複数 live 行があると margin/collateral 共有で会計破綻する。
-- 大小文字 / 空白のバリエーションで制約をすり抜けないよう、正規化を DB レベルで強制。
BEGIN;

-- 既存行を正規化。
UPDATE trading_accounts
SET exchange = lower(btrim(exchange, E' \t\n\r'))
WHERE exchange <> lower(btrim(exchange, E' \t\n\r'));

-- 正規化後に regex 違反 or live 重複があれば明示的に失敗させる。
-- そのまま ADD CONSTRAINT に進むと cryptic な constraint violation で
-- 停止するため、事前に check してわかりやすいエラーを出す。
DO $$
DECLARE
    bad_rows integer;
    dup_rows integer;
BEGIN
    SELECT COUNT(*) INTO bad_rows
    FROM trading_accounts
    WHERE exchange !~ '^[a-z0-9_]+$';
    IF bad_rows > 0 THEN
        RAISE EXCEPTION 'migration abort: % trading_accounts row(s) have exchange values that violate ^[a-z0-9_]+$ even after normalization. Fix data manually before re-running.', bad_rows;
    END IF;

    SELECT COUNT(*) INTO dup_rows FROM (
        SELECT exchange
        FROM trading_accounts
        WHERE account_type = 'live'
        GROUP BY exchange
        HAVING COUNT(*) > 1
    ) d;
    IF dup_rows > 0 THEN
        RAISE EXCEPTION 'migration abort: % exchange(s) have multiple live trading_accounts rows after normalization. Manually dedupe before re-running.', dup_rows;
    END IF;
END $$;

-- DB CHECK: exchange は lowercase alphanumeric + underscore のみ許可。
-- trim/lower 正規化の結果に加え、tab/newline 等の非可視文字も一括拒否。
-- 再実行時に "constraint already exists" で失敗しないよう、先に削除してから再作成。
ALTER TABLE trading_accounts
    DROP CONSTRAINT IF EXISTS trading_accounts_exchange_normalized;
ALTER TABLE trading_accounts
    ADD CONSTRAINT trading_accounts_exchange_normalized
    CHECK (exchange ~ '^[a-z0-9_]+$');

-- 正規化済み値での partial unique: exchange 毎に live は 1 件のみ。
CREATE UNIQUE INDEX IF NOT EXISTS trading_accounts_one_live_per_exchange
    ON trading_accounts (exchange)
    WHERE account_type = 'live';

COMMIT;
