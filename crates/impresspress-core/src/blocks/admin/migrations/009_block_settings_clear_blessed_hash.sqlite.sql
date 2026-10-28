-- Retire `block_settings.blessed_hash`. A database whose 001 predates
-- 2026-10-07 has the column and one created since does not, under the same
-- recorded list, so this file must leave both the same: it rebuilds the
-- table from the columns every database has, copied by name, and drops the
-- old one with whatever it held. (The basename predates this text, which
-- once emptied the column instead; the name is the file's key in the
-- migration ledger, so it stays.) The reasoning lives beside the constant
-- that embeds this file, in `migrations/mod.rs`.

CREATE TABLE impresspress__admin__block_settings_v2 (
    id                  TEXT PRIMARY KEY,
    block_name          TEXT NOT NULL UNIQUE,
    enabled             INTEGER NOT NULL DEFAULT 1,
    current_hash        TEXT NOT NULL DEFAULT '',
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    seed_defaults_hash  TEXT NOT NULL DEFAULT ''
);

INSERT INTO impresspress__admin__block_settings_v2
    (id, block_name, enabled, current_hash, created_at, updated_at, seed_defaults_hash)
SELECT id, block_name, enabled, current_hash, created_at, updated_at, seed_defaults_hash
  FROM impresspress__admin__block_settings;

DROP TABLE impresspress__admin__block_settings;

ALTER TABLE impresspress__admin__block_settings_v2
    RENAME TO impresspress__admin__block_settings;

CREATE UNIQUE INDEX impresspress__admin__block_settings_block_name_uniq
    ON impresspress__admin__block_settings (block_name);
