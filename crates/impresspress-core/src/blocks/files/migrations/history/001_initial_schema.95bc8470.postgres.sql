-- Initial files schema (Postgres parity — untested).
-- Solobase deploys SQLite/D1 today; this file is included for parity with
-- the auth-migrations pattern. Validate before enabling Postgres for files.

CREATE TABLE IF NOT EXISTS suppers_ai__files__buckets (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    public       BOOLEAN NOT NULL DEFAULT FALSE,
    created_by   TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_buckets_created_by
    ON suppers_ai__files__buckets (created_by);

CREATE TABLE IF NOT EXISTS suppers_ai__files__objects (
    id            TEXT PRIMARY KEY,
    bucket        TEXT NOT NULL,
    key           TEXT NOT NULL,
    size          BIGINT NOT NULL DEFAULT 0,
    content_type  TEXT NOT NULL DEFAULT 'application/octet-stream',
    status        TEXT NOT NULL DEFAULT 'complete',
    uploaded_by   TEXT NOT NULL DEFAULT '',
    uploaded_at   TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_objects_bucket_key
    ON suppers_ai__files__objects (bucket, key);
CREATE INDEX IF NOT EXISTS idx_objects_uploaded_by
    ON suppers_ai__files__objects (uploaded_by);

CREATE TABLE IF NOT EXISTS suppers_ai__files__views (
    id          TEXT PRIMARY KEY,
    bucket      TEXT NOT NULL,
    key         TEXT NOT NULL,
    user_id     TEXT NOT NULL DEFAULT '',
    viewed_at   TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_views_user_id_viewed_at
    ON suppers_ai__files__views (user_id, viewed_at);

CREATE TABLE IF NOT EXISTS suppers_ai__files__cloud_shares (
    id                 TEXT PRIMARY KEY,
    token              TEXT NOT NULL,
    bucket             TEXT NOT NULL,
    key                TEXT NOT NULL,
    created_by         TEXT NOT NULL DEFAULT '',
    expires_at         TEXT,
    access_count       INTEGER NOT NULL DEFAULT 0,
    max_access_count   INTEGER,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_cloud_shares_token
    ON suppers_ai__files__cloud_shares (token);
CREATE INDEX IF NOT EXISTS idx_cloud_shares_created_by
    ON suppers_ai__files__cloud_shares (created_by);

CREATE TABLE IF NOT EXISTS suppers_ai__files__cloud_access_logs (
    id           TEXT PRIMARY KEY,
    share_id     TEXT NOT NULL,
    accessed_at  TEXT,
    ip_address   TEXT NOT NULL DEFAULT '',
    user_agent   TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cloud_access_logs_share_id
    ON suppers_ai__files__cloud_access_logs (share_id);

CREATE TABLE IF NOT EXISTS suppers_ai__files__cloud_quotas (
    id                    TEXT PRIMARY KEY,
    user_id               TEXT NOT NULL,
    max_storage_bytes     BIGINT NOT NULL DEFAULT 1073741824,
    max_file_size_bytes   BIGINT NOT NULL DEFAULT 104857600,
    max_files_per_bucket  INTEGER NOT NULL DEFAULT 10000,
    reset_period_days     INTEGER NOT NULL DEFAULT 0,
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_cloud_quotas_user_id
    ON suppers_ai__files__cloud_quotas (user_id);
