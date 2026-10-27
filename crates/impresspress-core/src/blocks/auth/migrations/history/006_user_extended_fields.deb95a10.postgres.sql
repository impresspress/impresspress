-- Additive user columns layered onto the 001 baseline. Kept in a separate
-- migration (not folded into 001's CREATE TABLE) so existing databases pick
-- them up: `CREATE TABLE IF NOT EXISTS` is a no-op once the table exists, but
-- ALTER TABLE ADD COLUMN materializes the new columns.
ALTER TABLE suppers_ai__auth__users ADD COLUMN IF NOT EXISTS verification_token TEXT;
ALTER TABLE suppers_ai__auth__users ADD COLUMN IF NOT EXISTS last_verification_sent TEXT;
ALTER TABLE suppers_ai__auth__users ADD COLUMN IF NOT EXISTS last_login_at TEXT;
ALTER TABLE suppers_ai__auth__users ADD COLUMN IF NOT EXISTS name TEXT;
ALTER TABLE suppers_ai__auth__users ADD COLUMN IF NOT EXISTS disabled BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE suppers_ai__auth__users ADD COLUMN IF NOT EXISTS deleted_at TEXT;
