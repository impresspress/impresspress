-- Declare the two columns the password-reset flow writes on
-- `wafer_run__auth__users` (`repo::users::set_reset_token` and its
-- neighbours): the SHA-256 hex of the outstanding reset token, and its
-- expiry. No migration declared them before; until strict schema became the
-- only behaviour the runtime created them lazily on the first reset request,
-- and on a strict target (every Cloudflare deployment) that request failed.
--
-- A plain `ADD COLUMN`, on purpose. A database that ever served a reset
-- without strict schema (native or browser) already holds both columns,
-- created lazily; there this file fails whole inside its transaction and is
-- refused by name as drifted schema, and that database is recreated
-- (RELEASE.md). The reasoning is in the 017 note in `migrations/mod.rs`.

ALTER TABLE wafer_run__auth__users ADD COLUMN reset_token TEXT;
ALTER TABLE wafer_run__auth__users ADD COLUMN reset_token_expires TEXT;
