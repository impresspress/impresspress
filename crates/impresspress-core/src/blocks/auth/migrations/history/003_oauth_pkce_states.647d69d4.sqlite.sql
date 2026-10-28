-- OAuth PKCE state store (SEC-040).
--
-- Replaces the previous client-visible state JWT (which embedded the PKCE
-- code_verifier in plaintext). The client receives only an opaque random
-- `state_id`; the verifier + redirect_uri + provider sit server-side and
-- are looked up by state_id during the callback.
--
-- Single-use: the callback reads and deletes the row in one step. Rows
-- past `expires_at` are also treated as missing.
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
