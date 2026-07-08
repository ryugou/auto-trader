-- 取引所側 SL ストップ注文の ID (bitFlyer: parent_order_acceptance_id,
-- GMO: closeOrder の orderId)。dry_run (paper) は常に NULL。
ALTER TABLE trades ADD COLUMN stop_order_id TEXT;
