-- Kill Switch: 日次損失上限を超えた口座の新規エントリー停止。
-- halted_until が未来の間、entry signal は拒否される。close は常に許可。
ALTER TABLE trading_accounts
    ADD COLUMN halted_until TIMESTAMPTZ,
    ADD COLUMN halt_reason TEXT;
