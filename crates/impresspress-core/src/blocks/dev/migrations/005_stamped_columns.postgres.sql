-- Declare `updated_at` on the three dev tables that lacked it. The host
-- stamps `updated_at` on every typed write, and the dev repo updates all
-- three, so without the column each write fails where strict schema is on.
--
-- Two populations run this file under one recorded list: a database whose
-- tables match 001-004 exactly, and one whose writes already created
-- `updated_at` lazily, while strict schema was off. `IF NOT EXISTS` states
-- one end state for both; it tolerates no error. A lazily created
-- `updated_at` is exactly `updated_at TEXT` (the host stamps a string, and
-- lazy column-add typed a string column TEXT, nullable, no default), the
-- definition declared here, so the column is the same on both. The `UPDATE`
-- then fills every row from its own data (`created_at`, `imported_at` for
-- the seed record), replacing any lazily stamped value. `impresspress-native`'s
-- `tests/postgres_migration_history.rs` pins that identity on a lazily
-- created database.

ALTER TABLE impresspress__dev__generations ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__dev__generations SET updated_at = created_at;
ALTER TABLE impresspress__dev__builds ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__dev__builds SET updated_at = created_at;
ALTER TABLE impresspress__dev__seed_info ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__dev__seed_info SET updated_at = imported_at;
