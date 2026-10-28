-- Sliding-window rate-limit counters, one row per key (user id or IP).
-- Written only by the wasm32/D1 path in blocks/rate_limit.rs via the atomic
-- `OnConflict::WindowedCounter` upsert (native builds use the in-memory
-- UserRateLimiter); `key` is the upsert's ON CONFLICT target, so it must be
-- UNIQUE. New table: `CREATE TABLE IF NOT EXISTS` materializes it on both
-- fresh installs and existing databases (the table never pre-exists — before
-- this migration the counter path silently failed against a missing table).
CREATE TABLE IF NOT EXISTS suppers_ai__auth__rate_limits (
    id           TEXT PRIMARY KEY,
    key          TEXT NOT NULL UNIQUE,
    count        INTEGER NOT NULL,
    window_start INTEGER NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
