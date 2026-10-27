-- Empty `blessed_hash` on every block's row, so a build from before the
-- column was retired that is still serving logs schema drift instead of
-- re-applying its own migrations. The column is added where it is missing,
-- so every database has it. The reasoning lives beside the constant that
-- embeds this file, in `migrations/mod.rs`; a shipped migration is
-- hash-addressed over its whole text, so prose here cannot be corrected.

ALTER TABLE impresspress__admin__block_settings
    ADD COLUMN IF NOT EXISTS blessed_hash TEXT NOT NULL DEFAULT '';

UPDATE impresspress__admin__block_settings
SET blessed_hash = ''
WHERE blessed_hash <> '';
