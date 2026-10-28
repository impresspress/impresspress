-- User portal block schema (SQLite / D1).
--
-- Replaces the implicit `ensure_table` materialization for
-- `suppers_ai__userportal__buttons` (TEXT-only columns, no indexes) with
-- explicit DDL. Mirrors the `CollectionSchema::new(TABLE)` declaration in
-- `blocks/userportal/mod.rs`: `label`, `icon` (default "package"), `path`,
-- `sort_order` (int default 0), plus the standard `id`/`created_at`/
-- `updated_at` columns the helpers stamp.
--
-- A `sort_order` index makes the `load_buttons` ORDER BY usable without
-- a full scan as the table grows.
--
-- Mirrored to 001_userportal_schema.postgres.sql.

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
