-- 退役済み (active = FALSE) の live 口座は、同一 exchange への新しい live
-- 口座作成を妨げてはならない。20260418000001 で作った一意インデックスは
-- active を見ていないため、active を条件に加えて作り直す。
--
-- インデックス名は 20260418000001 と同じ (trading_accounts_one_live_per_exchange)
-- に揃える — crates/db/src/trading_accounts.rs の create_account がこの名前で
-- unique_violation (23505) を friendly message に変換しているため、名前を変える
-- とそのエラー変換が効かなくなる。
--
-- 適用後の影響と撤回時の注意:
-- 1. このマイグレーション適用後は、同一 exchange に退役済み (active = FALSE)
--    の live 口座を複数持てるようになる。
-- 2. リリースを撤回する場合、データの面では、前の版のコードは active でない
--    live 口座が存在する状態でも問題なく動く。インデックスは残してよい。
--    ただし、マイグレーションの履歴の面では、このファイルを含まない版の
--    アプリケーションは起動できない。起動時のマイグレーション検証が、
--    適用済みなのにファイルが無い版 (20261005000001) を検出して失敗するため
--    である。撤回するときは、次のどちらかを行う。
--    (a) 撤回先の版にも、このマイグレーションファイルを残す。
--    (b) 前の版を起動する前に、履歴からこの版だけを消す:
--          DELETE FROM _sqlx_migrations WHERE version = 20261005000001;
--        インデックスは作り直さなくてよい。
-- 3. インデックスを以前の条件 (active を見ない、exchange のみでユニーク) に
--    作り直す必要が生じた場合は、先に同一 exchange に live 口座 (active 問わず)
--    が複数存在しないか確認すること。確認用 SQL:
--
--      SELECT exchange, COUNT(*) FROM trading_accounts
--        WHERE account_type = 'live'
--        GROUP BY exchange
--        HAVING COUNT(*) > 1;
--
--    このクエリが1行も返さなければ、以前の条件のインデックスを安全に作り直せる。
BEGIN;

DROP INDEX IF EXISTS trading_accounts_one_live_per_exchange;

CREATE UNIQUE INDEX IF NOT EXISTS trading_accounts_one_live_per_exchange
    ON trading_accounts (exchange)
    WHERE account_type = 'live' AND active;

COMMIT;
