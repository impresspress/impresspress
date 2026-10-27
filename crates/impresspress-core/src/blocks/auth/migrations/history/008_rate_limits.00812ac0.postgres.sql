-- Sliding-window rate-limit counters, one row per key (user id or IP).
-- Written only by the wasm32/D1 path in blocks/rate_limit.rs via the atomic
-- `OnConflict::WindowedCounter` upsert (native builds use the in-memory
-- UserRateLimiter); `key` is the upsert's ON CONFLICT target, so it must be
-- UNIQUE. Timestamps are TIMESTAMPTZ (not TEXT as in older tables) because
-- the windowed upsert stamps them with a SQL `CURRENT_TIMESTAMP` expression,
-- which PostgreSQL will not assign to a TEXT column.
CREATE TABLE IF NOT EXISTS suppers_ai__auth__rate_limits (
    id           TEXT PRIMARY KEY,
    key          TEXT NOT NULL UNIQUE,
    count        BIGINT NOT NULL,
    window_start BIGINT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL
);
