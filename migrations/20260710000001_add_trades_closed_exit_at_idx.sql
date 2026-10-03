-- Kill Switch の日次実現損益集計 (realized_net_since) 用。
-- (account_id, status) 既存 index では exit_at で range-scan できず、
-- 口座の全 closed 行を毎シグナル走査してしまう。
CREATE INDEX trades_account_closed_exit_at_idx
    ON trades (account_id, exit_at)
    WHERE status = 'closed';
