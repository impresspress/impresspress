-- STRICT_SCHEMA prerequisite (wafer-run #313 consumer rollout).
--
-- `db::create` synthesizes a TEXT `id` and stamps `created_at`/`updated_at` on
-- every insert (and `db::update` re-stamps `updated_at`), but several 001/003/
-- 004/005/007 tables never declared those columns — they only ever existed
-- because the runtime's lazy column-add materialised them on first write.
-- STRICT_SCHEMA turns lazy column-add OFF, so those columns must be declared
-- explicitly or the next such write fails with "no such column".
--
-- Kept as a separate ALTER migration (not folded into the CREATE TABLE bodies)
-- so EXISTING databases pick the columns up: `CREATE TABLE IF NOT EXISTS` is a
-- no-op once the table exists, but `ALTER TABLE ADD COLUMN` materializes them.
--
-- All columns are nullable (no NOT NULL / DEFAULT): `db::create`/`db::update`
-- stamp the value explicitly on every new/updated row, and pre-existing rows
-- keep NULL — no read path treats these bookkeeping columns as significant, and
-- a NOT NULL add without a default would fail the backfill of existing rows.
-- SQLite has no `ADD COLUMN IF NOT EXISTS`; a re-run (or a column already
-- present from prior lazy column-add on a live database) raises "duplicate
-- column name", which `migration_helper` tolerates as an idempotent no-op.

ALTER TABLE wafer_run__auth__local_credentials ADD COLUMN updated_at TEXT;

ALTER TABLE wafer_run__auth__sessions ADD COLUMN id TEXT;
ALTER TABLE wafer_run__auth__sessions ADD COLUMN updated_at TEXT;

ALTER TABLE wafer_run__auth__orgs ADD COLUMN updated_at TEXT;

ALTER TABLE wafer_run__auth__provider_links ADD COLUMN created_at TEXT;
ALTER TABLE wafer_run__auth__provider_links ADD COLUMN updated_at TEXT;

ALTER TABLE wafer_run__auth__bootstrap_tokens ADD COLUMN updated_at TEXT;

ALTER TABLE wafer_run__auth__personal_access_tokens ADD COLUMN id TEXT;
ALTER TABLE wafer_run__auth__personal_access_tokens ADD COLUMN updated_at TEXT;

ALTER TABLE wafer_run__auth__oauth_pkce_states ADD COLUMN id TEXT;
ALTER TABLE wafer_run__auth__oauth_pkce_states ADD COLUMN updated_at TEXT;

ALTER TABLE wafer_run__auth__tokens ADD COLUMN updated_at TEXT;

ALTER TABLE wafer_run__auth__jwt_blocklist ADD COLUMN id TEXT;
ALTER TABLE wafer_run__auth__jwt_blocklist ADD COLUMN created_at TEXT;
ALTER TABLE wafer_run__auth__jwt_blocklist ADD COLUMN updated_at TEXT;

ALTER TABLE wafer_run__auth__api_keys ADD COLUMN updated_at TEXT;
