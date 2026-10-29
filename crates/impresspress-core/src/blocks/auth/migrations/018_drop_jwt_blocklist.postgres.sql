-- Drop the retired JWT blocklist, `wafer_run__auth__jwt_blocklist`, which
-- 005 created. Nothing reads or writes it (RELEASE.md, "Auth: the JWT
-- blocklist is retired"): an access token authenticates only while its
-- login family's session row is live (`crypto::verify_access_token`), and
-- logout and revoking a device delete that row, so a signed-out token is
-- refused without the blocklist. Its rows name tokens of sign-ins that have
-- already ended, so dropping them un-revokes nothing, and no session,
-- refresh token, API key or account changes: nobody is signed out. No
-- foreign key names the table in either direction. Its index goes with it.
--
-- A plain `DROP TABLE`, with no `IF EXISTS`, on purpose: 005 created the
-- table on every database that reaches this file, so a database without it
-- was adopted wrongly or edited by hand. There this file fails whole inside
-- its transaction and records nothing, rather than recording itself over a
-- schema the ledger does not describe. The 018 note in `migrations/mod.rs`
-- has the rest.

DROP TABLE wafer_run__auth__jwt_blocklist;
