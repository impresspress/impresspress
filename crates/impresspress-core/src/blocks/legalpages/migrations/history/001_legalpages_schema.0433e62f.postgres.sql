-- Legalpages block schema (Postgres parity — untested).
-- Solobase deploys SQLite/D1 today; this file is included for parity with
-- the auth/admin/files migration pattern. Validate before enabling
-- Postgres for legalpages.

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
