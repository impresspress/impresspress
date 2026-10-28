-- Initial vector schema (SQLite / D1).
--
-- Replaces the inline `ensure_registry()` CREATE TABLE DDL that previously
-- ran on every `create_index` call in `pages.rs`. CREATE TABLE IF NOT EXISTS
-- is a no-op against existing prod tables; the new index on `model` backs
-- the registry-by-model scans that the embed/query paths use.
--
-- Scope note: this migration ONLY covers the static block-owned `registry`
-- catalog. The per-index storage tables (`{prefixed}_meta`, `{prefixed}_fts`,
-- and the `vec0` virtual table) are intentionally NOT created here — their
-- names are user-supplied at runtime (`suppers_ai__vector__{user_name}`) and
-- their DDL lives in the upstream `wafer-run/vector` runtime block, which
-- the `vclient::create_index(ctx, cfg)` call dispatches to. Migrating those
-- to a static .sql file is impossible by construction; migrating them to a
-- typed `db::ddl` call at create-index time would just be a thinner wrapper
-- around what `wafer-run/vector` already does. See the corresponding
-- comment in `pages.rs::create_index`.
--
-- Mirrored to 001_vector_schema.postgres.sql.

CREATE TABLE IF NOT EXISTS suppers_ai__vector__registry (
    prefixed_name   TEXT PRIMARY KEY,
    model           TEXT NOT NULL DEFAULT '',
    dimensions      INTEGER NOT NULL DEFAULT 0,
    keyword_search  INTEGER NOT NULL DEFAULT 0
);
-- Backs `load_index_metadata` lookups by `prefixed_name` (PK already covers
-- this) and future model-rollup queries (e.g. "all indexes on bge-m3").
CREATE INDEX IF NOT EXISTS idx_vector_registry_model
    ON suppers_ai__vector__registry (model);
