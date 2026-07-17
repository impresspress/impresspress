-- OAuth PKCE state store (SEC-040). See the sqlite variant for full rationale.
-- Written via `db::create` (see repo/oauth_pkce.rs): synthesizes a TEXT `id`
-- and stamps `updated_at`, both declared here for STRICT_SCHEMA. `state_id`
-- stays the primary key / lookup column.
CREATE TABLE IF NOT EXISTS wafer_run__auth__oauth_pkce_states (
    state_id      TEXT PRIMARY KEY,
    id            TEXT,
    provider      TEXT NOT NULL,
    code_verifier TEXT NOT NULL,
    redirect_uri  TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT,
    expires_at    TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS wafer_run__auth__oauth_pkce_states_expires_at_idx
    ON wafer_run__auth__oauth_pkce_states (expires_at);
