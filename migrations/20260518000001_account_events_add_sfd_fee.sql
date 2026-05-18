-- account_events.event_type CHECK 制約に 'sfd_fee' を追加。
-- SFD (bitFlyer Crypto CFD の現物-FX 乖離手数料) accrual job が
-- paper account に対して fee 行を記録するため。受け取り SFD では
-- amount が正、支払 SFD では負 (overnight_fee と同じ符号規約)。
ALTER TABLE account_events
    DROP CONSTRAINT IF EXISTS account_events_event_type_check;

ALTER TABLE account_events
    ADD CONSTRAINT account_events_event_type_check
    CHECK (event_type IN (
        'margin_lock', 'margin_release', 'trade_open', 'trade_close',
        'overnight_fee', 'balance_sync', 'sfd_fee'
    ));
