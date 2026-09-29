-- Store an append-only WRAP grant in its own `append` column, as write = 0,
-- append = 1, and move any row that spelled it write = 2 onto that form. The
-- reasoning lives beside the constant that embeds this file, in
-- `migrations/mod.rs`; a shipped migration is hash-addressed over its whole
-- text, so prose here cannot be corrected.

ALTER TABLE impresspress__admin__wrap_grants
    ADD COLUMN IF NOT EXISTS append INTEGER NOT NULL DEFAULT 0;

UPDATE impresspress__admin__wrap_grants
SET write = 0, append = 1
WHERE write = 2;
