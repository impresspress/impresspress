-- Clear every stored OAuth provider access token. See
-- `014_clear_provider_access_tokens.sqlite.sql` for the full rationale; this
-- file is the PostgreSQL dialect of the same change.
UPDATE wafer_run__auth__provider_links SET access_token = '';
