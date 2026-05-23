-- account_events.event_type CHECK 制約に 'swap_fee' を追加。
-- GMO FX paper accrual job が daily swap point を記録するため。
-- 符号両対応 (apply_*_fee の規約: amount = -fee_amount なので
--  paper 払い → amount<0、paper 受取 → amount>0、sfd_fee 同規約)。
ALTER TABLE account_events
    DROP CONSTRAINT IF EXISTS account_events_event_type_check;

ALTER TABLE account_events
    ADD CONSTRAINT account_events_event_type_check
    CHECK (event_type IN (
        'margin_lock', 'margin_release', 'trade_open', 'trade_close',
        'overnight_fee', 'balance_sync', 'sfd_fee', 'swap_fee'
    ));
