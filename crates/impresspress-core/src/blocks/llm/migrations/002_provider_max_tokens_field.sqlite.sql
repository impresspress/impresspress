-- Per-provider override for the output-token budget's field name (SQLite / D1).
--
-- NULL means "whatever the provider's protocol implies"; the only other
-- values are the two wire spellings, 'max_tokens' and
-- 'max_completion_tokens'. Rationale lives in migrations/mod.rs beside this
-- file's test — a shipped .sql file is hash-addressed over its whole text,
-- comments included.
--
-- Mirrored to 002_provider_max_tokens_field.postgres.sql.

ALTER TABLE impresspress__llm__providers ADD COLUMN max_tokens_field TEXT;
