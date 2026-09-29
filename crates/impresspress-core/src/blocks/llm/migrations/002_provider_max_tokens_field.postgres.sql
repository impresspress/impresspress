-- Mirror of 002_provider_max_tokens_field.sqlite.sql for PostgreSQL.
--
-- Per-provider override for the output-token budget's field name. NULL means
-- "whatever the provider's protocol implies".

ALTER TABLE impresspress__llm__providers ADD COLUMN IF NOT EXISTS max_tokens_field TEXT;
