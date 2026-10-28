-- User portal block schema (PostgreSQL).
--
-- Mirror of 001_userportal_schema.sqlite.sql. INTEGER (not BOOLEAN) used
-- for any flag-style columns to match `record.i64_field(...)` reads in
-- block code (none here, but kept for consistency with the auth/admin
-- migrations). CREATE TABLE/INDEX IF NOT EXISTS is idempotent across
-- repeated `Init` lifecycle events.

CREATE TABLE IF NOT EXISTS suppers_ai__userportal__buttons (
    id          TEXT PRIMARY KEY,
    label       TEXT NOT NULL DEFAULT '',
    icon        TEXT NOT NULL DEFAULT 'package',
    path        TEXT NOT NULL DEFAULT '',
    sort_order  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS suppers_ai__userportal__buttons_sort_order_idx
    ON suppers_ai__userportal__buttons (sort_order);
