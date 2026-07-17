-- Refresh-token storage with explicit schema (replaces the legacy
-- `ensure_table`-materialized `wafer_run__auth__tokens` row layout).
--
-- SEC-032: refresh tokens are stored as SHA-256 hashes, never as raw JWTs.
-- SEC-039: family ID is preserved across rotation; `generation` increments
-- on each rotation; rotated rows are marked `revoked = 1` (not deleted) so
-- a subsequent attempt with the same token reveals a reuse attack.
--
-- Pre-prod posture (see workspace/.../active-development-can-wipe-prod-db.md):
-- existing rows from the legacy schema have an empty raw `token` column
-- value to us — we simply DROP the legacy table and start fresh. Users
-- log back in on next deploy.
DROP TABLE IF EXISTS wafer_run__auth__tokens;

CREATE TABLE IF NOT EXISTS wafer_run__auth__tokens (
    id           TEXT PRIMARY KEY,
    token_hash   TEXT NOT NULL,
    user_id      TEXT NOT NULL REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE,
    family       TEXT NOT NULL,
    generation   INTEGER NOT NULL DEFAULT 0,
    revoked      INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    expires_at   TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS wafer_run__auth__tokens_token_hash_uniq
    ON wafer_run__auth__tokens (token_hash);
CREATE INDEX IF NOT EXISTS wafer_run__auth__tokens_family_idx
    ON wafer_run__auth__tokens (family);
CREATE INDEX IF NOT EXISTS wafer_run__auth__tokens_user_id_idx
    ON wafer_run__auth__tokens (user_id);
