-- Declare the stamped columns the tickets block's typed writes name: the
-- host stamps `updated_at` on every typed write and `created_at` on every
-- create. `events` and `analyses` are created and have their expiry updated
-- (`updated_at`); the maintenance singleton is updated (`updated_at`) and
-- keeps a nullable `created_at`, which no production path writes (001 seeds
-- the row).
--
-- Two populations run this file under one recorded list: a database whose
-- tables match 001 exactly, and a native or browser database whose writes
-- already created some of these columns lazily (before strict schema was the
-- only behaviour). A plain `ADD COLUMN` would fail on the second, so each
-- table is rebuilt from the columns every database has, copied by name, and
-- the old table is dropped with whatever undeclared column it held. The new
-- columns are filled from the row's own data (`created_at`; the
-- maintenance row's `last_pruned_at`, and no `created_at`), so the result
-- does not depend on the population. No foreign key references these tables,
-- and the indexes the old tables carried are recreated.

CREATE TABLE impresspress__tickets__events_v2 (
    id              TEXT PRIMARY KEY,
    ticket_id       TEXT NOT NULL,
    event_type      TEXT NOT NULL,
    actor_type      TEXT NOT NULL
                        CHECK (actor_type IN ('public', 'admin', 'api', 'ai', 'system')),
    actor_id        TEXT NOT NULL DEFAULT '',
    body            TEXT NOT NULL DEFAULT '',
    metadata_json   TEXT NOT NULL DEFAULT '{}',
    expires_at      TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT
);

INSERT INTO impresspress__tickets__events_v2
    (id, ticket_id, event_type, actor_type, actor_id, body, metadata_json, expires_at, created_at, updated_at)
SELECT id, ticket_id, event_type, actor_type, actor_id, body, metadata_json, expires_at, created_at, created_at
  FROM impresspress__tickets__events;

DROP TABLE impresspress__tickets__events;

ALTER TABLE impresspress__tickets__events_v2
    RENAME TO impresspress__tickets__events;

CREATE INDEX idx_ticket_events_ticket_created
    ON impresspress__tickets__events (ticket_id, created_at);
CREATE INDEX idx_ticket_events_expires
    ON impresspress__tickets__events (expires_at);

CREATE TABLE impresspress__tickets__analyses_v2 (
    id                          TEXT PRIMARY KEY,
    ticket_id                   TEXT NOT NULL,
    source                      TEXT NOT NULL,
    model                       TEXT,
    prompt_version              TEXT NOT NULL DEFAULT '',
    summary                     TEXT NOT NULL,
    suggested_type_id           TEXT,
    suggested_priority          TEXT
                                    CHECK (suggested_priority IS NULL OR suggested_priority IN ('low', 'normal', 'high', 'urgent')),
    confidence                  REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    suggested_actions_json      TEXT NOT NULL DEFAULT '[]',
    expires_at                  TEXT,
    created_at                  TEXT NOT NULL,
    updated_at                  TEXT
);

INSERT INTO impresspress__tickets__analyses_v2
    (id, ticket_id, source, model, prompt_version, summary, suggested_type_id, suggested_priority, confidence, suggested_actions_json, expires_at, created_at, updated_at)
SELECT id, ticket_id, source, model, prompt_version, summary, suggested_type_id, suggested_priority, confidence, suggested_actions_json, expires_at, created_at, created_at
  FROM impresspress__tickets__analyses;

DROP TABLE impresspress__tickets__analyses;

ALTER TABLE impresspress__tickets__analyses_v2
    RENAME TO impresspress__tickets__analyses;

CREATE INDEX idx_ticket_analyses_ticket_created
    ON impresspress__tickets__analyses (ticket_id, created_at);
CREATE INDEX idx_ticket_analyses_expires
    ON impresspress__tickets__analyses (expires_at);

CREATE TABLE impresspress__tickets__maintenance_v2 (
    id                  TEXT PRIMARY KEY,
    last_pruned_day     TEXT NOT NULL DEFAULT '',
    last_pruned_at      TEXT,
    last_prune_error    TEXT NOT NULL DEFAULT '',
    audit_degraded      INTEGER NOT NULL DEFAULT 0 CHECK (audit_degraded IN (0, 1)),
    created_at          TEXT,
    updated_at          TEXT
);

INSERT INTO impresspress__tickets__maintenance_v2
    (id, last_pruned_day, last_pruned_at, last_prune_error, audit_degraded, created_at, updated_at)
SELECT id, last_pruned_day, last_pruned_at, last_prune_error, audit_degraded, NULL, last_pruned_at
  FROM impresspress__tickets__maintenance;

DROP TABLE impresspress__tickets__maintenance;

ALTER TABLE impresspress__tickets__maintenance_v2
    RENAME TO impresspress__tickets__maintenance;
