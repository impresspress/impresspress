-- STRICT_SCHEMA prerequisite (wafer-run #313 consumer rollout).
--
-- `stripe_events` is written via `db::create`, which stamps `updated_at`, but
-- 003's CREATE TABLE never declared it — it only existed via the runtime's lazy
-- column-add. STRICT_SCHEMA turns lazy column-add OFF, so declare it here.
-- Separate ALTER migration (not folded into 003) so EXISTING databases pick it
-- up: `CREATE TABLE IF NOT EXISTS` is a no-op once the table exists, but
-- `ALTER TABLE ADD COLUMN` materializes it. Nullable — `db::create` stamps the
-- value on new rows; pre-existing rows keep NULL. SQLite has no `ADD COLUMN IF
-- NOT EXISTS`; a re-run (or a column already present from lazy column-add)
-- raises "duplicate column name", tolerated by `migration_helper` as a no-op.

ALTER TABLE impresspress__products__stripe_events ADD COLUMN updated_at TEXT;
