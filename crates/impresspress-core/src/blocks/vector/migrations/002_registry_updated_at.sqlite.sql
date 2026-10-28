-- Declare `updated_at` on the registry. The registry is written with
-- `upsert`, which stamps nothing, and with `update_by_filters` (the
-- legacy-name rename, `legacy_names.rs`), which stamps `updated_at`; so that
-- is the one column the host writes that 001 lacked, and the rename fails
-- without it where strict schema is on.
--
-- Two populations run this file under one recorded list: a database whose
-- registry matches 001 exactly, and a native or browser database where a
-- rename already created `updated_at` lazily, while strict schema was off
-- there. A plain `ADD COLUMN` would fail on the second, so the table is
-- rebuilt from the columns every database has, copied by name, and the old
-- table is dropped with whatever undeclared column it held. `updated_at`
-- starts NULL on every row, whichever population, because the row carries
-- no time of its own to fill it from. No foreign key references the table.
--
-- The `model` index is recreated under a name scoped to the table. 001's
-- `idx_vector_registry_model` is not free on every database: one created
-- before 2026-07-13 holds it on the registry's earlier table (the tables
-- were renamed, their indexes were not), so its 001 never built the index
-- here, and recreating it under that name would fail. Under the new name
-- every database ends with the index, that one included.

CREATE TABLE impresspress__vector__registry_v2 (
    prefixed_name   TEXT PRIMARY KEY,
    model           TEXT NOT NULL DEFAULT '',
    dimensions      INTEGER NOT NULL DEFAULT 0,
    keyword_search  INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT
);

INSERT INTO impresspress__vector__registry_v2
    (prefixed_name, model, dimensions, keyword_search, updated_at)
SELECT prefixed_name, model, dimensions, keyword_search, NULL
  FROM impresspress__vector__registry;

DROP TABLE impresspress__vector__registry;

ALTER TABLE impresspress__vector__registry_v2
    RENAME TO impresspress__vector__registry;

CREATE INDEX idx_impresspress__vector__registry_model
    ON impresspress__vector__registry (model);
