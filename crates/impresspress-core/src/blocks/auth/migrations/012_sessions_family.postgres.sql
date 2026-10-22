-- [B12] The sweeper's bookkeeping table: the PostgreSQL dialect of
-- `012_sessions_family.sqlite.sql`, whose header says why this file no
-- longer drops and recreates `wafer_run__auth__sessions` (001 creates its
-- family-keyed layout).

CREATE TABLE IF NOT EXISTS wafer_run__auth__maintenance (
    id             TEXT PRIMARY KEY,
    last_swept_at  TEXT NOT NULL DEFAULT '',
    created_at     TEXT,
    updated_at     TEXT
);
INSERT INTO wafer_run__auth__maintenance (id)
VALUES ('singleton')
ON CONFLICT (id) DO NOTHING;
