-- Legalpages block schema (SQLite / D1).
--
-- Replaces the implicit `ensure_table` materialization (TEXT-only columns,
-- no indexes) for tables owned by `suppers-ai/legalpages`. CREATE TABLE
-- IF NOT EXISTS is a no-op against existing prod tables, but the
-- CREATE INDEX statement below adds the (doc_type, status) composite
-- so the public `terms` / `privacy` lookups stop scanning the table.
--
-- Mirrored to 001_legalpages_schema.postgres.sql.

CREATE TABLE IF NOT EXISTS suppers_ai__legalpages__documents (
    id            TEXT PRIMARY KEY,
    doc_type      TEXT NOT NULL,
    title         TEXT NOT NULL,
    content       TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT 'draft',
    version       INTEGER NOT NULL DEFAULT 1,
    created_by    TEXT NOT NULL DEFAULT '',
    published_at  TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_legalpages_documents_doc_type_status
    ON suppers_ai__legalpages__documents (doc_type, status);
