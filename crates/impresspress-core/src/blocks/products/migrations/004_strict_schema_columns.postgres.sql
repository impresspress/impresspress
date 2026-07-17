-- STRICT_SCHEMA prerequisite (wafer-run #313 consumer rollout). See the sqlite
-- twin for the rationale. Postgres supports `ADD COLUMN IF NOT EXISTS`, so a
-- re-run (or a column already present from lazy column-add) is a no-op.

ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS updated_at TEXT;
