-- Declare `updated_at` on the registry; why, and which two populations run
-- this file, is in the SQLite variant.
--
-- `IF NOT EXISTS` states one end state for both populations; it tolerates no
-- error. A lazily created `updated_at` is exactly `updated_at TEXT` (the host
-- stamps a string, and lazy column-add typed a string column TEXT, nullable,
-- no default), the definition declared here, so the column is the same on
-- both; the `UPDATE` then gives every row the NULL the SQLite rebuild
-- writes. `impresspress-native`'s
-- `tests/postgres_migration_history.rs` pins that identity on a lazily
-- created database.

ALTER TABLE impresspress__vector__registry ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__vector__registry SET updated_at = NULL;

-- The `model` index moves to the table-scoped name the SQLite rebuild gives
-- it (why is in the SQLite variant). 001 created `idx_vector_registry_model`
-- on every database: on the registry, or, on one created before 2026-07-13,
-- on the registry's earlier table, where it serves nothing. Either way it
-- goes, and the registry gets the index under its new name.
DROP INDEX idx_vector_registry_model;
CREATE INDEX idx_impresspress__vector__registry_model
    ON impresspress__vector__registry (model);
