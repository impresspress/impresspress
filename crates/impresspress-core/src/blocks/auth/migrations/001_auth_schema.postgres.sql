-- Legacy table drops — spec §6 migration 001 note.
DROP TABLE IF EXISTS iam_user_roles;
DROP TABLE IF EXISTS api_keys;
DROP TABLE IF EXISTS auth_sessions;
DROP TABLE IF EXISTS oauth_states;

CREATE TABLE IF NOT EXISTS wafer_run__auth__users (
    id              TEXT PRIMARY KEY,
    email           TEXT NOT NULL UNIQUE,
    display_name    TEXT NOT NULL,
    avatar_url      TEXT,
    role            TEXT NOT NULL DEFAULT 'user',
    email_verified  BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wafer_run__auth__local_credentials (
    id             TEXT PRIMARY KEY,
    user_id        TEXT NOT NULL UNIQUE REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE,
    password_hash  TEXT NOT NULL,
    must_reset     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

-- Written via `db::create` (see repo/provider_links.rs): stamps
-- `created_at`/`updated_at` (in addition to the domain `linked_at`), both
-- declared here for STRICT_SCHEMA.
CREATE TABLE IF NOT EXISTS wafer_run__auth__provider_links (
    id             TEXT PRIMARY KEY,
    provider       TEXT NOT NULL,
    provider_ref   TEXT NOT NULL,
    user_id        TEXT NOT NULL REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE,
    provider_login TEXT NOT NULL,
    access_token   TEXT NOT NULL,
    linked_at      TEXT NOT NULL,
    created_at     TEXT,
    updated_at     TEXT,
    UNIQUE (provider, provider_ref)
);

CREATE TABLE IF NOT EXISTS wafer_run__auth__orgs (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL UNIQUE,
    owner_user_id  TEXT REFERENCES wafer_run__auth__users(id) ON DELETE SET NULL,
    verified_via   TEXT,
    verified_ref   TEXT,
    is_reserved    BOOLEAN NOT NULL DEFAULT FALSE,
    created_at     TEXT NOT NULL,
    -- Stamped by `db::create`; nullable because migration 002 seeds reserved
    -- orgs via a raw INSERT that omits it.
    updated_at     TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS wafer_run__auth__orgs_verified_uniq
    ON wafer_run__auth__orgs (verified_via, verified_ref)
    WHERE is_reserved = FALSE;

-- Sessions (token_hash is sha256(raw), stored hex-encoded). Rows are written
-- via `db::create` (see repo/sessions.rs), which synthesizes a TEXT `id` and
-- stamps `created_at`/`updated_at`. Those columns are declared here so
-- STRICT_SCHEMA (which never lazily ADD-COLUMNs) has an authoritative schema.
-- Lookups are by `token_hash`; `id` identifies a row for update/delete.
CREATE TABLE IF NOT EXISTS wafer_run__auth__sessions (
    token_hash     BYTEA PRIMARY KEY,
    id             TEXT,
    user_id        TEXT NOT NULL REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE,
    created_at     TEXT NOT NULL,
    updated_at     TEXT,
    last_used_at   TEXT NOT NULL,
    expires_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS wafer_run__auth__sessions_user_id_idx
    ON wafer_run__auth__sessions (user_id);
CREATE INDEX IF NOT EXISTS wafer_run__auth__sessions_expires_at_idx
    ON wafer_run__auth__sessions (expires_at);

-- Written via `db::create` (see repo/pats.rs): synthesizes a TEXT `id` and
-- stamps `updated_at`, both declared here for STRICT_SCHEMA.
CREATE TABLE IF NOT EXISTS wafer_run__auth__personal_access_tokens (
    token_hash     BYTEA PRIMARY KEY,
    id             TEXT,
    user_id        TEXT NOT NULL REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    scopes         TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    updated_at     TEXT,
    last_used_at   TEXT,
    expires_at     TEXT
);
CREATE INDEX IF NOT EXISTS wafer_run__auth__personal_access_tokens_user_id_idx
    ON wafer_run__auth__personal_access_tokens (user_id);

CREATE TABLE IF NOT EXISTS wafer_run__auth__bootstrap_tokens (
    id             TEXT PRIMARY KEY,
    token_hash     TEXT NOT NULL UNIQUE,
    created_at     TEXT NOT NULL,
    updated_at     TEXT,
    expires_at     TEXT NOT NULL
);

