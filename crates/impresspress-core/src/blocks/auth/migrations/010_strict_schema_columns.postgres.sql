-- STRICT_SCHEMA prerequisite (wafer-run #313 consumer rollout). See the sqlite
-- twin for the full rationale. Postgres supports `ADD COLUMN IF NOT EXISTS`, so
-- re-runs (or columns already present from prior lazy column-add) are no-ops
-- without relying on error tolerance. All columns nullable: `db::create`/
-- `db::update` stamp the value on new/updated rows; pre-existing rows keep NULL.

ALTER TABLE wafer_run__auth__local_credentials ADD COLUMN IF NOT EXISTS updated_at TEXT;

ALTER TABLE wafer_run__auth__sessions ADD COLUMN IF NOT EXISTS id TEXT;
ALTER TABLE wafer_run__auth__sessions ADD COLUMN IF NOT EXISTS updated_at TEXT;

ALTER TABLE wafer_run__auth__orgs ADD COLUMN IF NOT EXISTS updated_at TEXT;

ALTER TABLE wafer_run__auth__provider_links ADD COLUMN IF NOT EXISTS created_at TEXT;
ALTER TABLE wafer_run__auth__provider_links ADD COLUMN IF NOT EXISTS updated_at TEXT;

ALTER TABLE wafer_run__auth__bootstrap_tokens ADD COLUMN IF NOT EXISTS updated_at TEXT;

ALTER TABLE wafer_run__auth__personal_access_tokens ADD COLUMN IF NOT EXISTS id TEXT;
ALTER TABLE wafer_run__auth__personal_access_tokens ADD COLUMN IF NOT EXISTS updated_at TEXT;

ALTER TABLE wafer_run__auth__oauth_pkce_states ADD COLUMN IF NOT EXISTS id TEXT;
ALTER TABLE wafer_run__auth__oauth_pkce_states ADD COLUMN IF NOT EXISTS updated_at TEXT;

ALTER TABLE wafer_run__auth__tokens ADD COLUMN IF NOT EXISTS updated_at TEXT;

ALTER TABLE wafer_run__auth__jwt_blocklist ADD COLUMN IF NOT EXISTS id TEXT;
ALTER TABLE wafer_run__auth__jwt_blocklist ADD COLUMN IF NOT EXISTS created_at TEXT;
ALTER TABLE wafer_run__auth__jwt_blocklist ADD COLUMN IF NOT EXISTS updated_at TEXT;

ALTER TABLE wafer_run__auth__api_keys ADD COLUMN IF NOT EXISTS updated_at TEXT;
