-- Declare `updated_at` on the three dev tables that lacked it; why, and
-- which two populations run this file, is in the SQLite variant.
--
-- `IF NOT EXISTS` states one end state for both populations; it tolerates no
-- error. A lazily created `updated_at` is exactly `updated_at TEXT` (the host
-- stamps a string, and lazy column-add typed a string column TEXT, nullable,
-- no default), the definition declared here, so the column is the same on
-- both; the `UPDATE` then gives every row the value the SQLite rebuild
-- copies. `impresspress-native`'s
-- `tests/postgres_migration_history.rs` pins that identity on a lazily
-- created database.

ALTER TABLE impresspress__dev__generations ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__dev__generations SET updated_at = created_at;
ALTER TABLE impresspress__dev__builds ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__dev__builds SET updated_at = created_at;
ALTER TABLE impresspress__dev__seed_info ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__dev__seed_info SET updated_at = imported_at;
