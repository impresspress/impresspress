-- Retire `block_settings.blessed_hash`. A database whose 001 predates
-- 2026-10-07 has the column and one created since does not, under the same
-- recorded list, so this file must leave both the same: `IF EXISTS` states
-- that end state for both, it does not tolerate an error. (The basename
-- predates this text, which once emptied the column instead; the name is the
-- file's key in the migration ledger, so it stays.) The reasoning lives
-- beside the constant that embeds this file, in `migrations/mod.rs`.

ALTER TABLE impresspress__admin__block_settings DROP COLUMN IF EXISTS blessed_hash;
