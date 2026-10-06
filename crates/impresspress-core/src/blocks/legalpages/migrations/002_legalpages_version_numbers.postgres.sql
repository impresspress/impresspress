-- Legalpages version numbers (PostgreSQL). Mirrors 002_legalpages_version_numbers.sqlite.sql,
-- plus the column type change SQLite cannot make without rebuilding the table.
--
-- Version numbers are the server's: a draft is unnumbered (0) until it is
-- published, and each number is taken at most once per document type.
-- Re-runnable: the type change is a no-op on INTEGER, both updates are
-- idempotent and the index is IF NOT EXISTS.

-- A table created before migration 001 (the implicit materialisation) has a
-- TEXT version column; numbers must compare as numbers. A no-op on INTEGER.
ALTER TABLE impresspress__legalpages__documents
    ALTER COLUMN version TYPE INTEGER USING version::integer;

-- Drafts used to be stored as version 1.
UPDATE impresspress__legalpages__documents SET version = 0 WHERE status = 'draft';

-- A publish could reuse a number. In each clash the published row keeps it,
-- else the most recently updated row (then the greatest id); the others
-- become unnumbered.
UPDATE impresspress__legalpages__documents SET version = 0
WHERE version > 0 AND EXISTS (
    SELECT 1 FROM impresspress__legalpages__documents other
    WHERE other.doc_type = impresspress__legalpages__documents.doc_type
      AND other.version = impresspress__legalpages__documents.version
      AND other.id <> impresspress__legalpages__documents.id
      AND (
          (other.status = 'published' AND impresspress__legalpages__documents.status <> 'published')
          OR (
              (other.status = 'published') = (impresspress__legalpages__documents.status = 'published')
              AND (
                  other.updated_at > impresspress__legalpages__documents.updated_at
                  OR (other.updated_at = impresspress__legalpages__documents.updated_at
                      AND other.id > impresspress__legalpages__documents.id)
              )
          )
      )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_legalpages_documents_doc_type_version
    ON impresspress__legalpages__documents (doc_type, version)
    WHERE version > 0;
