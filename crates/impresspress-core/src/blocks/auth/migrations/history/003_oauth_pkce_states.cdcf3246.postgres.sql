-- OAuth PKCE state store (SEC-040). See the sqlite variant for full rationale.
CREATE TABLE IF NOT EXISTS suppers_ai__auth__oauth_pkce_states (
    state_id      TEXT PRIMARY KEY,
    provider      TEXT NOT NULL,
    code_verifier TEXT NOT NULL,
    redirect_uri  TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    expires_at    TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS suppers_ai__auth__oauth_pkce_states_expires_at_idx
    ON suppers_ai__auth__oauth_pkce_states (expires_at);
