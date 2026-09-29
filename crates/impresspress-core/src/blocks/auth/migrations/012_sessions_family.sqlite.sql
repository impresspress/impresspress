-- [B12] A session row is a login family, not an access token.
--
-- Before this migration `wafer_run__auth__sessions` was keyed by
-- `sha256(access_token)`: one row per issuance, so a browser tab wrote a new
-- row roughly every 30 minutes, each living 30 days, none deleted on logout,
-- and every one of them shown on `/b/userportal/sessions` as a separate
-- device. The table is now keyed by the refresh-rotation `family` — one row
-- per device, touched on rotation, deleted on logout and on revoke, expiring
-- exactly when the refresh row it mirrors does.
--
-- The old rows cannot be converted: their key is a hash of a token that has
-- long since expired, and no family can be recovered from it. They are UX
-- rows, not credentials — nothing authenticates against this table — so
-- dropping them signs nobody out. `CREATE TABLE IF NOT EXISTS` is a no-op on
-- an existing table, which is why the drop is explicit rather than an edit to
-- `001_auth_schema`.
--
-- Migrations re-run in full whenever the block's SQL hash changes, so this
-- drop fires again on any future auth migration. `sessions::touch` reports
-- the rows it affected and `issue_tokens_and_cookie` inserts when it affected
-- none, so a device whose row went away re-appears on its next token refresh
-- instead of vanishing from the list until the user signs in again. That same
-- fallback is what re-materialises the families that already exist when this
-- migration first lands.
DROP TABLE IF EXISTS wafer_run__auth__sessions;

CREATE TABLE IF NOT EXISTS wafer_run__auth__sessions (
    family         TEXT PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE,
    -- "password", "oauth.<provider>", "bootstrap" — the claim the refresh
    -- token preserves across rotation, so the device list can say how the
    -- session was established.
    auth_method    TEXT NOT NULL DEFAULT '',
    created_at     TEXT NOT NULL,
    last_used_at   TEXT NOT NULL,
    expires_at     TEXT NOT NULL,
    -- `db::create` synthesizes an `id` and stamps `updated_at` on every
    -- insert; both are declared here (rather than added by a later ALTER the
    -- way `010_strict_schema_columns` had to) because this table is created
    -- fresh. Nullable for the same reason 010 gives: no read path treats
    -- these bookkeeping columns as significant.
    id             TEXT,
    updated_at     TEXT
);
CREATE INDEX IF NOT EXISTS wafer_run__auth__sessions_user_id_idx
    ON wafer_run__auth__sessions (user_id);
CREATE INDEX IF NOT EXISTS wafer_run__auth__sessions_expires_at_idx
    ON wafer_run__auth__sessions (expires_at);

-- [B12] The sweeper's singleton bookkeeping row, mirroring
-- `impresspress__tickets__maintenance`. `auth::maintenance::sweep_if_due`
-- reads `last_swept_at` to decide whether the hourly window has elapsed and
-- writes it back after a pass, so token issuance can prune the four
-- expiry-bearing auth tables without doing it on every login.
--
-- Deliberately a table this block owns rather than a row in
-- `impresspress__admin__variables`: WRAP grants are per table, so putting the
-- stamp there would mean granting `impresspress/auth-ui` write access to the
-- table that holds `WAFER_RUN__AUTH__JWT_SECRET`. The auth wildcard grant
-- auth-ui already has covers this table for free.
CREATE TABLE IF NOT EXISTS wafer_run__auth__maintenance (
    id             TEXT PRIMARY KEY,
    last_swept_at  TEXT NOT NULL DEFAULT '',
    created_at     TEXT,
    updated_at     TEXT
);
INSERT INTO wafer_run__auth__maintenance (id)
VALUES ('singleton')
ON CONFLICT (id) DO NOTHING;
