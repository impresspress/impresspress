-- LLM block schema (SQLite / D1).
--
-- Replaces the implicit `ensure_table` materialization (TEXT-only columns,
-- no indexes) for tables owned by `suppers-ai/llm`. CREATE TABLE IF NOT
-- EXISTS is a no-op against existing prod tables, but the CREATE INDEX
-- statements below add the missing UNIQUE/indexes so the in-band lookups
-- (`thread_id`, `enabled`, `name`) stop scanning the whole table.
--
-- Mirrored to 001_llm_schema.postgres.sql.

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
