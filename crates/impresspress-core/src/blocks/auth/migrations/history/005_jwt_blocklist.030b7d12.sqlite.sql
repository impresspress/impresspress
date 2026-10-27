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
CREATE TABLE IF NOT EXISTS suppers_ai__auth__jwt_blocklist (
    jti         TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL,
    revoked_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS suppers_ai__auth__jwt_blocklist_expires_at_idx
    ON suppers_ai__auth__jwt_blocklist (expires_at);
