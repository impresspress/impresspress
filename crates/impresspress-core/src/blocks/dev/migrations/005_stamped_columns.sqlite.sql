-- Declare `updated_at` on the three dev tables that lacked it. The host
-- stamps `updated_at` on every typed write, and the dev repo updates all
-- three, so without the column each write would fail under strict schema.
--
-- Two populations run this file under one recorded list: a database whose
-- tables match 001-004 exactly, and a browser database whose writes already
-- created `updated_at` lazily (before strict schema was the only behaviour).
-- A plain `ADD COLUMN` would fail on the second, so each table is rebuilt
-- from the columns every database has, copied by name, and the old table is
-- dropped with whatever undeclared column it held. `updated_at` is filled
-- from the row's own data (`created_at`, `imported_at` for the seed record),
-- so the result does not depend on the population. No foreign key references
-- these tables, and the indexes the old tables carried are recreated.

CREATE TABLE impresspress__dev__generations_v2 (
    id                  TEXT PRIMARY KEY,
    parent_id           TEXT,
    status              TEXT NOT NULL
                            CHECK (status IN ('staged', 'validating', 'activating', 'active', 'failed', 'superseded')),
    cause               TEXT NOT NULL
                            CHECK (cause IN ('site_write', 'site_delete', 'block_compile', 'block_remove', 'rollback', 'seed')),
    site_manifest_json  TEXT NOT NULL,
    block_manifest_json TEXT NOT NULL,
    manifest_sha256     TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    activated_at        TEXT,
    failure_message     TEXT,
    updated_at          TEXT
);

INSERT INTO impresspress__dev__generations_v2
    (id, parent_id, status, cause, site_manifest_json, block_manifest_json, manifest_sha256, created_at, activated_at, failure_message, updated_at)
SELECT id, parent_id, status, cause, site_manifest_json, block_manifest_json, manifest_sha256, created_at, activated_at, failure_message, created_at
  FROM impresspress__dev__generations;

DROP TABLE impresspress__dev__generations;

ALTER TABLE impresspress__dev__generations_v2
    RENAME TO impresspress__dev__generations;

CREATE INDEX idx_impresspress__dev__generations_created
    ON impresspress__dev__generations(created_at);

CREATE TABLE impresspress__dev__builds_v2 (
    id                      TEXT PRIMARY KEY,
    block_name              TEXT NOT NULL,
    source_manifest_sha256  TEXT NOT NULL,
    artifact_sha256         TEXT NOT NULL,
    block_info_json         TEXT NOT NULL,
    diagnostics_json        TEXT NOT NULL,
    compiler_version        TEXT NOT NULL,
    status                  TEXT NOT NULL
                                CHECK (status IN ('staged', 'valid', 'invalid')),
    created_at              TEXT NOT NULL,
    artifact_bytes          INTEGER NOT NULL DEFAULT 0,
    updated_at              TEXT
);

INSERT INTO impresspress__dev__builds_v2
    (id, block_name, source_manifest_sha256, artifact_sha256, block_info_json, diagnostics_json, compiler_version, status, created_at, artifact_bytes, updated_at)
SELECT id, block_name, source_manifest_sha256, artifact_sha256, block_info_json, diagnostics_json, compiler_version, status, created_at, artifact_bytes, created_at
  FROM impresspress__dev__builds;

DROP TABLE impresspress__dev__builds;

ALTER TABLE impresspress__dev__builds_v2
    RENAME TO impresspress__dev__builds;

CREATE INDEX idx_impresspress__dev__builds_created
    ON impresspress__dev__builds(created_at);

CREATE TABLE impresspress__dev__seed_info_v2 (
    singleton_id          INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    template              TEXT,
    suggested_prompt      TEXT,
    guide_markdown        TEXT,
    imported_at           TEXT,
    llms_text             TEXT,
    llms_published_sha256 TEXT,
    updated_at            TEXT
);

INSERT INTO impresspress__dev__seed_info_v2
    (singleton_id, template, suggested_prompt, guide_markdown, imported_at, llms_text, llms_published_sha256, updated_at)
SELECT singleton_id, template, suggested_prompt, guide_markdown, imported_at, llms_text, llms_published_sha256, imported_at
  FROM impresspress__dev__seed_info;

DROP TABLE impresspress__dev__seed_info;

ALTER TABLE impresspress__dev__seed_info_v2
    RENAME TO impresspress__dev__seed_info;
