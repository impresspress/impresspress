-- Give every upload reservation of an object row a token of its own. The
-- reasoning lives beside the constant that embeds this file, in
-- `migrations/mod.rs`; a shipped migration is hash-addressed over its whole
-- text, so prose here cannot be corrected.

ALTER TABLE impresspress__files__objects ADD COLUMN IF NOT EXISTS claim_id TEXT;
