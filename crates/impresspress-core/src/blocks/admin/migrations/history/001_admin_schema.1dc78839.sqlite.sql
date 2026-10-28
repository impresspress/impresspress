-- Admin block schema (SQLite / D1).
--
-- Replaces the implicit `ensure_table` materialization (TEXT-only columns,
-- no UNIQUE/indexes) for tables owned by `suppers-ai/admin`. CREATE TABLE
-- IF NOT EXISTS is a no-op against existing prod tables, but the
-- CREATE INDEX statements below add the missing UNIQUE/indexes so
-- WHERE-by-key lookups stop scanning the whole table.
--
-- Mirrored to 001_admin_schema.postgres.sql.

-- Variables (config store) -------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__admin__variables (
    id          TEXT PRIMARY KEY,
    key         TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    value       TEXT NOT NULL DEFAULT '',
    warning     TEXT NOT NULL DEFAULT '',
    sensitive   INTEGER NOT NULL DEFAULT 0,
    updated_by  TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS suppers_ai__admin__variables_key_uniq
    ON suppers_ai__admin__variables (key);

-- Roles --------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__admin__roles (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    permissions TEXT NOT NULL DEFAULT '[]',
    is_system   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS suppers_ai__admin__roles_name_uniq
    ON suppers_ai__admin__roles (name);

-- Permissions -------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__admin__permissions (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    resource   TEXT NOT NULL DEFAULT '',
    actions    TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS suppers_ai__admin__permissions_name_uniq
    ON suppers_ai__admin__permissions (name);

-- User-role assignments ---------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__admin__user_roles (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL,
    role        TEXT NOT NULL,
    assigned_at TEXT,
    assigned_by TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__admin__user_roles_user_id_idx
    ON suppers_ai__admin__user_roles (user_id);

-- Audit logs --------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__admin__audit_logs (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL DEFAULT '',
    action     TEXT NOT NULL,
    resource   TEXT NOT NULL DEFAULT '',
    ip_address TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__admin__audit_logs_created_at_idx
    ON suppers_ai__admin__audit_logs (created_at);

-- Request logs ------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__admin__request_logs (
    id            TEXT PRIMARY KEY,
    flow_id       TEXT NOT NULL DEFAULT '',
    method        TEXT NOT NULL DEFAULT '',
    path          TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT '',
    status_code   INTEGER NOT NULL DEFAULT 0,
    duration_ms   INTEGER NOT NULL DEFAULT 0,
    error_message TEXT NOT NULL DEFAULT '',
    client_ip     TEXT NOT NULL DEFAULT '',
    user_id       TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__admin__request_logs_created_at_idx
    ON suppers_ai__admin__request_logs (created_at);

-- Storage access logs ----------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__admin__storage_access_logs (
    id           TEXT PRIMARY KEY,
    source_block TEXT NOT NULL DEFAULT '',
    operation    TEXT NOT NULL DEFAULT '',
    path         TEXT NOT NULL DEFAULT '',
    status       TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__admin__storage_access_logs_created_at_idx
    ON suppers_ai__admin__storage_access_logs (created_at);

-- Block settings (per-block on/off toggles) ------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__admin__block_settings (
    id         TEXT PRIMARY KEY,
    block_name TEXT NOT NULL UNIQUE,
    enabled    INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS suppers_ai__admin__block_settings_block_name_uniq
    ON suppers_ai__admin__block_settings (block_name);

-- WRAP grants -----------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__admin__wrap_grants (
    id            TEXT PRIMARY KEY,
    grantee       TEXT NOT NULL,
    resource      TEXT NOT NULL,
    write         INTEGER NOT NULL DEFAULT 0,
    resource_type TEXT NOT NULL DEFAULT '',
    description   TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__admin__wrap_grants_grantee_idx
    ON suppers_ai__admin__wrap_grants (grantee);
