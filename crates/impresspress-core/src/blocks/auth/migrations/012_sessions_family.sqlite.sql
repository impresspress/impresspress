-- [B12] The sweeper's bookkeeping table. What this file used to do first is
-- recorded here because a live database may still carry its effects.
--
-- It used to re-key `wafer_run__auth__sessions` from `sha256(access_token)`
-- to the refresh-rotation `family`, by `DROP TABLE IF EXISTS` and a fresh
-- `CREATE`. Auth migrations re-run as a SET whenever any one of them
-- changes, so that DROP fired again on every later auth schema change and
-- emptied the table. Once a session row became a login family's liveness
-- (`blocks::auth::repo::sessions`: a refresh whose family has no row is
-- refused, and nothing re-creates one), every such re-run signed every
-- device out. THIS FILE MUST NOT DROP THE TABLE.
--
-- The family-keyed layout is now created by `001_auth_schema`, so a fresh
-- database never has the old one. A database that already applied this file
-- has the family-keyed table, which 001's `CREATE TABLE IF NOT EXISTS` and
-- the rest of the set leave as it is, rows included
-- (`re_run_survival_tests::session_rows_survive_a_full_re_run`). A database
-- last migrated before this file first existed still holds the old layout
-- and is not converted; RELEASE.md says to recreate it.

-- [B12] The sweeper's singleton bookkeeping row, mirroring
-- `impresspress__tickets__maintenance`. `auth::maintenance::sweep_if_due`
-- reads `last_swept_at` to decide whether the hourly window has elapsed and
-- writes it back after a pass, so token issuance can prune the four
-- expiry-bearing auth tables without doing it on every login.
--
-- Deliberately a table this block owns rather than a row in
-- `impresspress__admin__variables`: WRAP grants are per table, so putting the
-- stamp there would mean granting `impresspress/auth-ui` write access to the
-- table that holds `WAFER_RUN__AUTH__JWT_SECRET`. The auth wildcard grant
-- auth-ui already has covers this table for free.
CREATE TABLE IF NOT EXISTS wafer_run__auth__maintenance (
    id             TEXT PRIMARY KEY,
    last_swept_at  TEXT NOT NULL DEFAULT '',
    created_at     TEXT,
    updated_at     TEXT
);
INSERT INTO wafer_run__auth__maintenance (id)
VALUES ('singleton')
ON CONFLICT (id) DO NOTHING;
