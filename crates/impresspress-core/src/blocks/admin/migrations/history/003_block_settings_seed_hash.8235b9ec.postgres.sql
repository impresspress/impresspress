-- Mirror of 003_block_settings_seed_hash.sqlite.sql for PostgreSQL.
--
-- Adds a `seed_defaults_hash` column to suppers_ai__admin__block_settings
-- so `admin::settings::seed_defaults` can hash-gate itself the same way
-- `migration_helper::apply_if_blessed` gates DDL.
--
-- Spec: docs/superpowers/specs/2026-05-14-config-snapshot-and-migration-gate-design.md
--       § "Hash-gate seed_defaults like migrations" (PR 3)

ALTER TABLE suppers_ai__admin__block_settings
    ADD COLUMN IF NOT EXISTS seed_defaults_hash TEXT NOT NULL DEFAULT '';
