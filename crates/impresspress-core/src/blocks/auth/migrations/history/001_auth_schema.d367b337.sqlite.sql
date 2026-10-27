-- Legacy table drops — spec §6 migration 001 note.
DROP TABLE IF EXISTS iam_user_roles;
DROP TABLE IF EXISTS api_keys;
DROP TABLE IF EXISTS auth_sessions;
DROP TABLE IF EXISTS oauth_states;

-- Users (spec §3)
CREATE TABLE IF NOT EXISTS suppers_ai__auth__users (
    id            TEXT PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL,
    avatar_url    TEXT,
    role          TEXT NOT NULL DEFAULT 'user',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

-- Local credentials (empty for OAuth-only users)
CREATE TABLE IF NOT EXISTS suppers_ai__auth__local_credentials (
    user_id        TEXT PRIMARY KEY REFERENCES suppers_ai__auth__users(id) ON DELETE CASCADE,
    password_hash  TEXT NOT NULL,
    must_reset     INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL
);

-- Provider links (github/google/microsoft)
CREATE TABLE IF NOT EXISTS suppers_ai__auth__provider_links (
    provider       TEXT NOT NULL,
    provider_ref   TEXT NOT NULL,
    user_id        TEXT NOT NULL REFERENCES suppers_ai__auth__users(id) ON DELETE CASCADE,
    provider_login TEXT NOT NULL,
    access_token   TEXT NOT NULL,
    linked_at      TEXT NOT NULL,
    PRIMARY KEY (provider, provider_ref)
);

-- Orgs
CREATE TABLE IF NOT EXISTS suppers_ai__auth__orgs (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL UNIQUE,
    owner_user_id  TEXT REFERENCES suppers_ai__auth__users(id) ON DELETE SET NULL,
    verified_via   TEXT,
    verified_ref   TEXT,
    is_reserved    INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS suppers_ai__auth__orgs_verified_uniq
    ON suppers_ai__auth__orgs (verified_via, verified_ref)
    WHERE is_reserved = 0;

-- Sessions (token_hash is sha256(raw))
CREATE TABLE IF NOT EXISTS suppers_ai__auth__sessions (
    token_hash     BLOB PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES suppers_ai__auth__users(id) ON DELETE CASCADE,
    created_at     TEXT NOT NULL,
    last_used_at   TEXT NOT NULL,
    expires_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__auth__sessions_user_id_idx
    ON suppers_ai__auth__sessions (user_id);
CREATE INDEX IF NOT EXISTS suppers_ai__auth__sessions_expires_at_idx
    ON suppers_ai__auth__sessions (expires_at);

-- Personal access tokens
CREATE TABLE IF NOT EXISTS suppers_ai__auth__personal_access_tokens (
    token_hash     BLOB PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES suppers_ai__auth__users(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    scopes         TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    last_used_at   TEXT,
    expires_at     TEXT
);
CREATE INDEX IF NOT EXISTS suppers_ai__auth__personal_access_tokens_user_id_idx
    ON suppers_ai__auth__personal_access_tokens (user_id);

-- CLI exchange codes (15-min one-time)
CREATE TABLE IF NOT EXISTS suppers_ai__auth__cli_exchange_codes (
    code_hash      BLOB PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES suppers_ai__auth__users(id) ON DELETE CASCADE,
    created_at     TEXT NOT NULL,
    expires_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__auth__cli_exchange_codes_expires_at_idx
    ON suppers_ai__auth__cli_exchange_codes (expires_at);

-- Bootstrap tokens (first-run admin seeding)
CREATE TABLE IF NOT EXISTS suppers_ai__auth__bootstrap_tokens (
    token_hash     BLOB PRIMARY KEY,
    created_at     TEXT NOT NULL,
    expires_at     TEXT NOT NULL
);
