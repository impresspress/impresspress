-- LLM block schema (PostgreSQL).
--
-- Mirror of 001_llm_schema.sqlite.sql. INTEGER (not BOOLEAN) is used for
-- boolean-like columns to match `record.i64_field(...)` reads in block
-- code. CREATE TABLE IF NOT EXISTS makes this idempotent across repeated
-- `Init` lifecycle events.

-- Per-thread provider/model overrides ------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__llm__settings (
    id             TEXT PRIMARY KEY,
    thread_id      TEXT NOT NULL,
    provider_block TEXT NOT NULL DEFAULT '',
    model          TEXT NOT NULL DEFAULT '',
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__llm__settings_thread_id_idx
    ON suppers_ai__llm__settings (thread_id);

-- Provider configurations ------------------------------------------------
-- Secrets are NOT stored here: providers reference an entry in
-- `suppers_ai__admin__variables` via `key_var`. See `schema.rs` for the
-- ProviderConfig <-> row encoding (api_key is runtime-only and never
-- persisted).
CREATE TABLE IF NOT EXISTS suppers_ai__llm__providers (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    protocol   TEXT NOT NULL,
    endpoint   TEXT NOT NULL,
    key_var    TEXT,
    models     TEXT NOT NULL DEFAULT '[]',
    enabled    INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS suppers_ai__llm__providers_name_uniq
    ON suppers_ai__llm__providers (name);
CREATE INDEX IF NOT EXISTS suppers_ai__llm__providers_enabled_idx
    ON suppers_ai__llm__providers (enabled);
