-- JWT blocklist (SEC-042).
--
-- Logout inserts the current request's `jti` into this table. JWT-bearer
-- validation in `pipeline::handle_request` (via `extract_auth_meta`) checks
-- the table after structural JWT validation; a hit → request continues as
-- unauthenticated (same posture as an invalid token).
--
-- `expires_at` matches the original JWT's `exp` (ISO-8601) so a background
-- prune can drop rows that can no longer be presented anyway. Pruning is
-- best-effort and not required for correctness — the table grows at most
-- one row per logout per access-token lifetime.
-- Written via `db::create` (see repo/jwt_blocklist.rs): synthesizes a TEXT
-- `id` and stamps `created_at`/`updated_at`, all declared here for
-- STRICT_SCHEMA. `jti` stays the primary key / lookup column.
CREATE TABLE IF NOT EXISTS wafer_run__auth__jwt_blocklist (
    jti         TEXT PRIMARY KEY,
    id          TEXT,
    user_id     TEXT NOT NULL,
    revoked_at  TEXT NOT NULL,
    created_at  TEXT,
    updated_at  TEXT,
    expires_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS wafer_run__auth__jwt_blocklist_expires_at_idx
    ON wafer_run__auth__jwt_blocklist (expires_at);
