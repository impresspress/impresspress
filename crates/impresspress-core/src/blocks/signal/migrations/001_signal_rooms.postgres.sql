-- Initial signal schema (Postgres parity — untested).
-- Impresspress deploys SQLite/D1 today; this file is included for parity with
-- the auth/files/messages-migrations pattern. Validate before enabling
-- Postgres for the signal block.
--
-- One row per signalling room: the host's offer, then the guest's answer,
-- then nothing. Bounded by `expires_at`; reads treat a past expiry as a
-- missing row and delete it, and `open_room` sweeps, so there is no
-- background job this store's correctness depends on.
--
-- `code` stays the real primary key. `id` and `updated_at` are declared
-- alongside it, nullable, for `db::create`/`db::update_by_filters_count`'s
-- shared defaults (synthesized `id`, stamped `updated_at`) — required under
-- `WAFER_RUN__DATABASE__STRICT_SCHEMA`, where lazy column-add is off. Same
-- shape `auth/migrations/003_oauth_pkce_states.postgres.sql` ends up in
-- after its own `010_strict_schema_columns.postgres.sql` retrofit.
CREATE TABLE IF NOT EXISTS impresspress__signal__rooms (
    code       TEXT PRIMARY KEY,
    offer_sdp  TEXT NOT NULL,
    answer_sdp TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    id         TEXT,
    updated_at TEXT
);
-- The sweep in `open_room` is the only query that is not by primary key.
CREATE INDEX IF NOT EXISTS impresspress__signal__rooms_expires_at_idx
    ON impresspress__signal__rooms (expires_at);
