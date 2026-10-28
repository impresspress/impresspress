-- JWT blocklist (SEC-042). See the sqlite variant for full rationale.
CREATE TABLE IF NOT EXISTS suppers_ai__auth__jwt_blocklist (
    jti         TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL,
    revoked_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS suppers_ai__auth__jwt_blocklist_expires_at_idx
    ON suppers_ai__auth__jwt_blocklist (expires_at);
