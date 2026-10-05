-- 退役済み (active = FALSE) の live 口座は、同一 exchange への新しい live
-- 口座作成を妨げてはならない。20260418000001 で作った一意インデックスは
-- active を見ていないため、active を条件に加えて作り直す。
--
-- インデックス名は 20260418000001 と同じ (trading_accounts_one_live_per_exchange)
-- に揃える — crates/db/src/trading_accounts.rs の create_account がこの名前で
-- unique_violation (23505) を friendly message に変換しているため、名前を変える
-- とそのエラー変換が効かなくなる。
BEGIN;

DROP INDEX IF EXISTS trading_accounts_one_live_per_exchange;

CREATE UNIQUE INDEX IF NOT EXISTS trading_accounts_one_live_per_exchange
    ON trading_accounts (exchange)
    WHERE account_type = 'live' AND active;

COMMIT;
