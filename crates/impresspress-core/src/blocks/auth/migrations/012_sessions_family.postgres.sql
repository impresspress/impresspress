-- [B12] A session row is a login family, not an access token.
-- See `012_sessions_family.sqlite.sql` for the full rationale; this file is
-- the PostgreSQL dialect of the same change.
DROP TABLE IF EXISTS wafer_run__auth__sessions;

CREATE TABLE IF NOT EXISTS wafer_run__auth__sessions (
    family         TEXT PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE,
    auth_method    TEXT NOT NULL DEFAULT '',
    created_at     TEXT NOT NULL,
    last_used_at   TEXT NOT NULL,
    expires_at     TEXT NOT NULL,
    id             TEXT,
    updated_at     TEXT
);
CREATE INDEX IF NOT EXISTS wafer_run__auth__sessions_user_id_idx
    ON wafer_run__auth__sessions (user_id);
CREATE INDEX IF NOT EXISTS wafer_run__auth__sessions_expires_at_idx
    ON wafer_run__auth__sessions (expires_at);

CREATE TABLE IF NOT EXISTS wafer_run__auth__maintenance (
    id             TEXT PRIMARY KEY,
    last_swept_at  TEXT NOT NULL DEFAULT '',
    created_at     TEXT,
    updated_at     TEXT
);
INSERT INTO wafer_run__auth__maintenance (id)
VALUES ('singleton')
ON CONFLICT (id) DO NOTHING;
