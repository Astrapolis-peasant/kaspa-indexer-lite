-- Add tx_count column to chain_blocks and backfill from transactions table.
-- Run AFTER adding the column but BEFORE restarting the indexer.
--
-- The indexer's VCP inserts chain_blocks with ON CONFLICT DO NOTHING,
-- so existing rows won't be overwritten — this migration fills the gap.

ALTER TABLE chain_blocks ADD COLUMN IF NOT EXISTS tx_count SMALLINT;

-- Backfill: count accepted transactions per chain block.
-- Uses the idx_transactions_accepted_by index for efficient grouping.
UPDATE chain_blocks cb
SET tx_count = sub.cnt
FROM (
    SELECT accepted_by, COUNT(*)::smallint AS cnt
    FROM transactions
    WHERE accepted_by IS NOT NULL
    GROUP BY accepted_by
) sub
WHERE cb.hash = sub.accepted_by
  AND cb.tx_count IS NULL;

-- Blocks with no accepted transactions get 0.
UPDATE chain_blocks SET tx_count = 0 WHERE tx_count IS NULL;
