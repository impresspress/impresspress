-- Refresh-token storage with explicit schema (replaces the legacy
-- `ensure_table`-materialized `wafer_run__auth__tokens` row layout).
--
-- SEC-032: refresh tokens are stored as SHA-256 hashes, never as raw JWTs.
-- SEC-039: family ID is preserved across rotation; `generation` increments
-- on each rotation; rotated rows are marked `revoked = 1` (not deleted) so
-- a subsequent attempt with the same token reveals a reuse attack.
--
-- THIS FILE MUST NOT DROP THE TABLE. It used to open with
-- `DROP TABLE IF EXISTS wafer_run__auth__tokens;`, to discard the legacy
-- row layout on the one upgrade that introduced this schema. But auth
-- migrations re-run AS A SET whenever any one of them changes, so that DROP
-- ran again on every later schema change and deleted every live refresh
-- token with it: `auth_ui::api::refresh` refuses a token whose row is gone,
-- so every signed-in user was silently logged out within one access-token
-- lifetime, on an upgrade that had nothing to do with tokens.
-- `re_run_survival_tests::refresh_tokens_survive_a_full_re_run` pins that
-- they survive.
--
-- What that costs: a database still carrying the PRE-004 layout (one that
-- has not applied this file even once — impossible for any deployment that
-- has booted since 005 landed, since the set re-runs together) keeps its
-- legacy table, and `CREATE TABLE IF NOT EXISTS` is a no-op on it, so the
-- columns below are missing and the first refresh write fails "no such
-- column". The remedy there is one statement — `DROP TABLE
-- wafer_run__auth__tokens` by hand, then re-run migrations — and it costs
-- that deployment exactly what this DROP used to cost EVERY deployment on
-- EVERY auth schema change.

CREATE TABLE IF NOT EXISTS wafer_run__auth__tokens (
    id           TEXT PRIMARY KEY,
    token_hash   TEXT NOT NULL,
    user_id      TEXT NOT NULL REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE,
    family       TEXT NOT NULL,
    generation   INTEGER NOT NULL DEFAULT 0,
    revoked      INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    expires_at   TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS wafer_run__auth__tokens_token_hash_uniq
    ON wafer_run__auth__tokens (token_hash);
CREATE INDEX IF NOT EXISTS wafer_run__auth__tokens_family_idx
    ON wafer_run__auth__tokens (family);
CREATE INDEX IF NOT EXISTS wafer_run__auth__tokens_user_id_idx
    ON wafer_run__auth__tokens (user_id);
