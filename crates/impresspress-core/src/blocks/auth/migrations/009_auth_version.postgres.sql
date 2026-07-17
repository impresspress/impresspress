-- Per-user auth_version counter (P2c: CODE_REVIEW_2026-07-16, "Access JWTs
-- outlive account and role changes"). See the sqlite twin for the full
-- rationale.
ALTER TABLE wafer_run__auth__users ADD COLUMN IF NOT EXISTS auth_version INTEGER NOT NULL DEFAULT 0;
