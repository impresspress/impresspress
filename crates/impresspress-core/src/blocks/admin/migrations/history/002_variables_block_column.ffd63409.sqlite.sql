-- Add a `block` column to suppers_ai__admin__variables for indexed per-block
-- lookup by D1ConfigSource (lazy-init redesign).
--
-- Format: SCREAMING_SNAKE block prefix derived from the `key` column's first
-- two `__`-delimited segments. e.g. key `SUPPERS_AI__AUTH__JWT_SECRET` ->
-- block `SUPPERS_AI__AUTH`. Keys without two segments (legacy or shared
-- SOLOBASE_SHARED__*) leave `block` NULL.
--
-- Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §6

ALTER TABLE suppers_ai__admin__variables ADD COLUMN block TEXT;

-- Backfill existing rows. SQLite-only: find the second `__` in `key` and
-- take the substring up to (but not including) that position. Keys with
-- fewer than two `__` get NULL.
UPDATE suppers_ai__admin__variables
SET block = CASE
    WHEN instr(substr(key, instr(key, '__') + 2), '__') > 0
        THEN substr(key, 1, instr(key, '__') + 1 + instr(substr(key, instr(key, '__') + 2), '__') - 1)
    ELSE NULL
END
WHERE block IS NULL;

CREATE INDEX IF NOT EXISTS suppers_ai__admin__variables_block_idx
    ON suppers_ai__admin__variables (block);
