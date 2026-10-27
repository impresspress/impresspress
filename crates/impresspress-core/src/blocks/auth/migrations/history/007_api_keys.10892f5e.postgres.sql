-- API keys. New table, so `CREATE TABLE IF NOT EXISTS` materializes it on both
-- fresh installs and existing databases (the table never pre-exists).
CREATE TABLE IF NOT EXISTS suppers_ai__auth__api_keys (
    id             TEXT PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES suppers_ai__auth__users(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    key_hash       TEXT NOT NULL UNIQUE,
    key_prefix     TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    expires_at     TEXT,
    revoked_at     TEXT
);
CREATE INDEX IF NOT EXISTS suppers_ai__auth__api_keys_user_id_idx
    ON suppers_ai__auth__api_keys (user_id);
