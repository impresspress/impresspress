-- Initial signal schema (Postgres parity — untested).
-- Impresspress deploys SQLite/D1 today; this file is included for parity with
-- the auth/files/messages-migrations pattern. Validate before enabling
-- Postgres for the signal block.
--
-- One row per signalling room: the host's offer, then the guest's answer,
-- then nothing. Bounded by `expires_at`; reads treat a past expiry as a
-- missing row and delete it, and `open_room` sweeps, so there is no
-- background job this store's correctness depends on.
CREATE TABLE IF NOT EXISTS impresspress__signal__rooms (
    code       TEXT PRIMARY KEY,
    offer_sdp  TEXT NOT NULL,
    answer_sdp TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
-- The sweep in `open_room` is the only query that is not by primary key.
CREATE INDEX IF NOT EXISTS impresspress__signal__rooms_expires_at_idx
    ON impresspress__signal__rooms (expires_at);
