-- Declare `updated_at` on the registry. The registry is written with
-- `upsert`, which stamps nothing, and with `update_by_filters` (the
-- legacy-name rename, `legacy_names.rs`), which stamps `updated_at`; so that
-- is the one column the host writes that 001 lacked, and the rename fails
-- without it where strict schema is on.
--
-- Two populations run this file under one recorded list: a database whose
-- registry matches 001 exactly, and one where a rename already created
-- `updated_at` lazily, while strict schema was off. `IF NOT EXISTS` states
-- one end state for both; it tolerates no error. A lazily created
-- `updated_at` is exactly `updated_at TEXT` (the host stamps a string, and
-- lazy column-add typed a string column TEXT, nullable, no default), the
-- definition declared here, so the column is the same on both. The `UPDATE`
-- then sets every row's to NULL, since a row carries no time of its own to
-- fill it from. `impresspress-native`'s `tests/postgres_migration_history.rs`
-- pins that identity on a lazily created database.

ALTER TABLE impresspress__vector__registry ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__vector__registry SET updated_at = NULL;

-- The `model` index moves to a name scoped to the table, the one the SQLite
-- variant's rebuild gives it. 001's `idx_vector_registry_model` is not
-- always the registry's: index names are schema-wide, and on a database
-- created before 2026-07-13 the registry's earlier table holds that name
-- (the tables were renamed, their indexes were not), where it serves
-- nothing. Every database has it on one table or the other, so it goes, and
-- the registry gets the index under its new name.
DROP INDEX idx_vector_registry_model;
CREATE INDEX idx_impresspress__vector__registry_model
    ON impresspress__vector__registry (model);
