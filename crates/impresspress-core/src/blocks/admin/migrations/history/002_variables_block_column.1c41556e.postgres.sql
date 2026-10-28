-- Mirror of 002_variables_block_column.sqlite.sql for PostgreSQL.
--
-- Adds a `block` column to suppers_ai__admin__variables for indexed
-- per-block lookup by D1ConfigSource (lazy-init redesign). Postgres
-- exposes `strpos` (1-indexed, returns 0 when not found) and
-- `substring(s FROM start FOR length)` — semantically equivalent to
-- the SQLite `instr`/`substr` formula in the sibling file.
--
-- Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §6

ALTER TABLE suppers_ai__admin__variables ADD COLUMN IF NOT EXISTS block TEXT;

-- Backfill existing rows. Find the second `__` in `key` and take the
-- substring up to (but not including) that position. Keys with fewer
-- than two `__` get NULL.
UPDATE suppers_ai__admin__variables
SET block = CASE
    WHEN strpos(substring(key FROM strpos(key, '__') + 2), '__') > 0
        THEN substring(key FROM 1 FOR strpos(key, '__') + 1 + strpos(substring(key FROM strpos(key, '__') + 2), '__') - 1)
    ELSE NULL
END
WHERE block IS NULL;

CREATE INDEX IF NOT EXISTS suppers_ai__admin__variables_block_idx
    ON suppers_ai__admin__variables (block);
