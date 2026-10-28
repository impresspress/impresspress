-- Declare the stamped columns the tickets block's typed writes name: the
-- host stamps `updated_at` on every typed write and `created_at` on every
-- create. `events` and `analyses` gain `updated_at`; the maintenance
-- singleton gains `updated_at` and a nullable `created_at`, which no
-- production path writes (001 seeds the row). Without them these writes
-- fail where strict schema is on.
--
-- Two populations run this file under one recorded list: a database whose
-- tables match 001 exactly, and one whose writes already created some of
-- these columns lazily, while strict schema was off. `IF NOT EXISTS` states
-- one end state for both; it tolerates no error. A lazily created stamp
-- column is exactly `<column> TEXT` (the host stamps a string, and lazy
-- column-add typed a string column TEXT, nullable, no default), the
-- definition declared here, so the column is the same on both. The
-- `UPDATE`s then fill every row from its own data (`created_at`; the
-- maintenance row's `last_pruned_at`, and no `created_at`), replacing any
-- lazily stamped value. `impresspress-native`'s
-- `tests/postgres_migration_history.rs` pins that identity on a lazily
-- created database.

ALTER TABLE impresspress__tickets__events ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__tickets__events SET updated_at = created_at;
ALTER TABLE impresspress__tickets__analyses ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__tickets__analyses SET updated_at = created_at;
ALTER TABLE impresspress__tickets__maintenance ADD COLUMN IF NOT EXISTS created_at TEXT;
ALTER TABLE impresspress__tickets__maintenance ADD COLUMN IF NOT EXISTS updated_at TEXT;
UPDATE impresspress__tickets__maintenance SET created_at = NULL, updated_at = last_pruned_at;
