-- Additive user columns layered onto the 001 baseline. Kept in a separate
-- migration (not folded into 001's CREATE TABLE) so existing databases pick
-- them up: `CREATE TABLE IF NOT EXISTS` is a no-op once the table exists, but
-- ALTER TABLE ADD COLUMN materializes the new columns.
--
-- SQLite has no `ADD COLUMN IF NOT EXISTS`; re-runs raise "duplicate column
-- name", which `migration_helper` tolerates as an idempotent no-op.
ALTER TABLE suppers_ai__auth__users ADD COLUMN verification_token TEXT;
ALTER TABLE suppers_ai__auth__users ADD COLUMN last_verification_sent TEXT;
ALTER TABLE suppers_ai__auth__users ADD COLUMN last_login_at TEXT;
ALTER TABLE suppers_ai__auth__users ADD COLUMN name TEXT;
ALTER TABLE suppers_ai__auth__users ADD COLUMN disabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE suppers_ai__auth__users ADD COLUMN deleted_at TEXT;
