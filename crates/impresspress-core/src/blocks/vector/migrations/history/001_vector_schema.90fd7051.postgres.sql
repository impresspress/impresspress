-- Initial vector schema (Postgres parity — untested).
-- Solobase deploys SQLite/D1 today; this file is included for parity with
-- the auth/files/messages-migrations pattern. The vector block itself is
-- SQLite-only (sqlite-vec extension), so this Postgres path will only
-- become reachable if/when a Postgres-native vector backend lands. Validate
-- before enabling Postgres for the vector block.

CREATE TABLE IF NOT EXISTS suppers_ai__vector__registry (
    prefixed_name   TEXT PRIMARY KEY,
    model           TEXT NOT NULL DEFAULT '',
    dimensions      INTEGER NOT NULL DEFAULT 0,
    keyword_search  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_vector_registry_model
    ON suppers_ai__vector__registry (model);
