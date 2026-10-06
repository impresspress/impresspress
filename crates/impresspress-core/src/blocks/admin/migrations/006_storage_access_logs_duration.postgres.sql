-- Give a storage access its duration as a column of its own, rather than
-- inside its status text. The reasoning lives beside the constant that embeds
-- this file, in `migrations/mod.rs`; a shipped migration is hash-addressed
-- over its whole text, so prose here cannot be corrected.

ALTER TABLE impresspress__admin__storage_access_logs
    ADD COLUMN IF NOT EXISTS duration_ms INTEGER;
