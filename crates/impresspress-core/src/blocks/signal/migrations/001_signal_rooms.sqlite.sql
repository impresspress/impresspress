-- One row per signalling room: the host's offer, then the guest's answer,
-- then nothing. Bounded by `expires_at`; reads treat a past expiry as a
-- missing row and delete it, and `open_room` sweeps, so there is no
-- background job this store's correctness depends on.
--
-- `code` stays the real primary key — nothing about the lookup-by-code or
-- collision-on-create story changes. `id` and `updated_at` are declared
-- alongside it, nullable, for `db::create`/`db::update_by_filters_count`'s
-- shared defaults, which unconditionally synthesize a UUID `id` (no table
-- here auto-generates one — `code TEXT PRIMARY KEY` doesn't qualify) and
-- stamp `updated_at` on every write. Under the default (non-strict) config
-- those land via lazy `ALTER TABLE ADD COLUMN` and this never mattered; under
-- `WAFER_RUN__DATABASE__STRICT_SCHEMA` (how production Cloudflare/D1 is
-- configured — see `impresspress-cloudflare/src/database.rs`) lazy column-add
-- is off by design and a write against an undeclared column fails loudly, so
-- the columns must be declared here instead. Same shape
-- `auth/migrations/003_oauth_pkce_states.sqlite.sql` ends up in after its own
-- `010_strict_schema_columns.sqlite.sql` retrofit — declared directly here
-- since this table has no pre-existing rows to retrofit.
--
-- Mirrored to 001_signal_rooms.postgres.sql.
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
