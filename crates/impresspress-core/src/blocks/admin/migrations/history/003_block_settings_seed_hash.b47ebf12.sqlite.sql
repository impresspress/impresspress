-- Add a `seed_defaults_hash` column to suppers_ai__admin__block_settings.
--
-- Stores a SHA-256 hex digest of the deterministic seed payload that
-- `admin::settings::seed_defaults` last applied to the `variables` table.
-- On cold start, when the cached hash matches the current
-- `shared_config_vars()` hash, `seed_defaults` short-circuits before
-- issuing any D1 query — dropping the residual ~100 D1 reads/day attributed
-- to that function in prod (the bulk `list_all` introduced by PR 2 of the
-- 2026-05-14 config-snapshot spec).
--
-- Spec: docs/superpowers/specs/2026-05-14-config-snapshot-and-migration-gate-design.md
--       § "Hash-gate seed_defaults like migrations" (PR 3)

ALTER TABLE suppers_ai__admin__block_settings
    ADD COLUMN seed_defaults_hash TEXT NOT NULL DEFAULT '';
