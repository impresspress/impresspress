-- See 003_refresh_tokens.sqlite.sql for the rationale (SEC-032/SEC-039).
DROP TABLE IF EXISTS suppers_ai__auth__tokens;

CREATE TABLE IF NOT EXISTS suppers_ai__auth__tokens (
    id           TEXT PRIMARY KEY,
    token_hash   TEXT NOT NULL,
    user_id      TEXT NOT NULL REFERENCES suppers_ai__auth__users(id) ON DELETE CASCADE,
    family       TEXT NOT NULL,
    generation   BIGINT NOT NULL DEFAULT 0,
    revoked      BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TEXT NOT NULL,
    expires_at   TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS suppers_ai__auth__tokens_token_hash_uniq
    ON suppers_ai__auth__tokens (token_hash);
CREATE INDEX IF NOT EXISTS suppers_ai__auth__tokens_family_idx
    ON suppers_ai__auth__tokens (family);
CREATE INDEX IF NOT EXISTS suppers_ai__auth__tokens_user_id_idx
    ON suppers_ai__auth__tokens (user_id);
