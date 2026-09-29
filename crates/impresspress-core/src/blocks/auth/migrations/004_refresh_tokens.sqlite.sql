-- Refresh-token storage with explicit schema (replaces the legacy
-- `ensure_table`-materialized row layout, which only ever existed under the
-- table's earlier name, `suppers_ai__auth__tokens`).
--
-- SEC-032: refresh tokens are stored as SHA-256 hashes, never as raw JWTs.
-- SEC-039: family ID is preserved across rotation; `generation` increments
-- on each rotation; rotated rows are marked `revoked = 1` (not deleted) so
-- a subsequent attempt with the same token reveals a reuse attack.
--
-- THIS FILE MUST NOT DROP THE TABLE. It used to open with
-- a `DROP TABLE IF EXISTS` of this table, written to discard the legacy
-- row layout on the one upgrade that introduced this schema. But auth
-- migrations re-run AS A SET whenever any one of them changes, so that DROP
-- ran again on every later schema change and deleted every live refresh
-- token with it: `auth_ui::api::refresh` refuses a token whose row is gone,
-- so every signed-in user was silently logged out within one access-token
-- lifetime, on an upgrade that had nothing to do with tokens.
-- `re_run_survival_tests::refresh_tokens_survive_a_full_re_run` pins that
-- they survive.
--
-- Removing it strands nobody. The legacy layout only ever existed as
-- `suppers_ai__auth__tokens`, and the auth tables moved to the
-- `wafer_run__auth__*` names with no data migration, so every
-- `wafer_run__auth__tokens` there is was created by the statement below.

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
