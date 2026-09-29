-- Who proved the address on a users row, as distinct from whether the login
-- policy currently demands one. See `013_email_proof.sqlite.sql` for the full
-- rationale; this file is the PostgreSQL dialect of the same change.
ALTER TABLE wafer_run__auth__users ADD COLUMN IF NOT EXISTS email_verified_by TEXT;
