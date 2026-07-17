-- JWT blocklist (SEC-042). See the sqlite variant for full rationale.
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
