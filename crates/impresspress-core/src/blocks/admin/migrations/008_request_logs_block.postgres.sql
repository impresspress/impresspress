-- Store the block each logged request was addressed to, so the network page
-- groups and totals by it in SQL. The
-- reasoning lives beside the constant that embeds this file, in
-- `migrations/mod.rs`; a shipped migration is hash-addressed over its whole
-- text, so prose here cannot be corrected.

ALTER TABLE impresspress__admin__request_logs
    ADD COLUMN IF NOT EXISTS block TEXT NOT NULL DEFAULT '';

UPDATE impresspress__admin__request_logs
SET block = split_part(substr(path, 4), '/', 1)
WHERE substr(path, 1, 3) = '/b/' AND block = '';

CREATE INDEX IF NOT EXISTS impresspress__admin__request_logs_block_idx
    ON impresspress__admin__request_logs (block);
