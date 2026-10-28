-- Declare the stamped columns the tickets block's typed writes name; why,
-- and which two populations run this file, is in the SQLite variant.
--
-- `IF NOT EXISTS` states one end state for both populations; it tolerates no
-- error. A lazily created stamp column is exactly `<column> TEXT` (the host
-- stamps a string, and lazy column-add typed a string column TEXT, nullable,
-- no default), the definition declared here, so the column is the same on
-- both; the `UPDATE`s then give every row the values the SQLite rebuild
-- copies. `impresspress-native`'s
-- `tests/postgres_migration_history.rs` pins that identity on a lazily
-- created database.

ALTER TABLE impresspress__tickets__events ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__tickets__events SET updated_at = created_at;
ALTER TABLE impresspress__tickets__analyses ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__tickets__analyses SET updated_at = created_at;
ALTER TABLE impresspress__tickets__maintenance ADD COLUMN IF NOT EXISTS created_at TEXT;
ALTER TABLE impresspress__tickets__maintenance ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__tickets__maintenance SET created_at = NULL, updated_at = last_pruned_at;
