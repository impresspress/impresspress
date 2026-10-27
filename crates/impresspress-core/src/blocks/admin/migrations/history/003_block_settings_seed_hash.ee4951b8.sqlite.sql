-- Add a `seed_defaults_hash` column to impresspress__admin__block_settings.
--
-- Stores a SHA-256 hex digest of the deterministic seed payload that
-- `admin::settings::seed_defaults` last applied to the `variables` table.
-- On cold start, when the cached hash matches the current
-- `shared_config_vars()` hash, `seed_defaults` short-circuits before
-- issuing any D1 query — dropping the residual ~100 D1 reads/day attributed
-- to that function in prod — those reads came from the bulk `list_all` the
-- config-snapshot work had added to `seed_defaults`.
--
-- The gate is the same shape `migration_helper::apply_if_blessed` uses for
-- DDL: hash the payload, compare against the stored digest, skip on a match.

ALTER TABLE impresspress__admin__block_settings
    ADD COLUMN seed_defaults_hash TEXT NOT NULL DEFAULT '';
