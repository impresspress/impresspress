-- Declare the two columns the password-reset flow writes on
-- `wafer_run__auth__users` (`repo::users::set_reset_token` and its
-- neighbours): the SHA-256 hex of the outstanding reset token, and its
-- expiry. No migration declared them before. Where strict schema is on
-- (every Cloudflare deployment) the first reset request failed; where it was
-- off (native and the browser, until it is on everywhere) that request
-- created both columns lazily.
--
-- A plain `ADD COLUMN`, on purpose: the table is the parent of cascading
-- foreign keys, so it cannot be rebuilt. A database that ever served a reset
-- while strict schema was off already holds both columns; there this file
-- fails whole inside its transaction, is refused by name as drifted schema,
-- and the database is recreated (RELEASE.md). The reasoning is in the 017
-- note in `migrations/mod.rs`.

ALTER TABLE wafer_run__auth__users ADD COLUMN reset_token TEXT;
ALTER TABLE wafer_run__auth__users ADD COLUMN reset_token_expires TEXT;
