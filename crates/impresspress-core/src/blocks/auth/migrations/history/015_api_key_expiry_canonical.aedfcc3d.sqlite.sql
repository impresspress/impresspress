-- Put `wafer_run__auth__api_keys.expires_at` into the one format the column
-- is meant to hold, and revoke the keys whose stored expiry is not a
-- timestamp at all.
--
-- `POST /b/auth/api/api-keys` used to store the caller's `expires_at` string
-- exactly as sent, and the lookup compared it to the clock as TEXT. Both
-- halves were wrong for a value that is not a UTC RFC 3339 timestamp:
-- `2026-09-23T20:00:00+09:00` is 11:00 UTC but sorts after `…T12:00:00Z`, so
-- a key an hour dead still authenticated, and `never` sorts after every
-- timestamp there will ever be, so a key minted with it never expired. The
-- endpoint now refuses anything it cannot read and stores the instant in
-- `%Y-%m-%dT%H:%M:%SZ`; this is the same repair for the rows already there.
--
-- The reader is fail-closed as of this release (`ApiKeyRow::is_expired`
-- treats an unparseable expiry as expired), so no row here is dangerous by
-- the time this runs. What this migration adds is the column's one format,
-- and a revocation an operator can see instead of a key that silently
-- stopped working.
--
-- The boundary between the two arms is `repo::parse_iso`, whose accepted and
-- rejected spellings are pinned by
-- `repo::tests::parse_iso_reads_rfc_3339_and_nothing_else`. Neither arm
-- parses anything: every test below is a literal `substr` comparison, so a
-- stored string cannot make this migration fail, and a migration that fails
-- is never stamped and re-runs (and re-fails) on every boot.
--
-- Adding this file changes the auth block's SQL hash, so the upgrade that
-- applies it re-runs every auth migration, 012's sessions-table drop
-- included. RELEASE.md spells out what that costs.

-- Arm 1: a UTC expiry, respelled. `Z`, `z`, `+00:00` and `-00:00` all name
-- the same offset, and characters 1-10 and 12-19 are the date and the time
-- whichever of them was used, so this rewrite cannot move the instant. The
-- exact `IN` list is deliberate: a wildcard would also match a string that
-- merely ENDS in `Z`, and rewriting `…T12:00:00<junk>Z` into a valid
-- timestamp would bring a key the reader refuses back to life.
UPDATE wafer_run__auth__api_keys
   SET expires_at = substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z',
       updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
 WHERE substr(expires_at, 1, 19) GLOB
       '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
   AND substr(expires_at, 20) IN ('Z', 'z', '+00:00', '-00:00')
   AND expires_at <> substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z';

-- Arm 2: an expiry that names no instant. The key is already dead to the
-- reader; this records that as a revocation, and gives the column a value it
-- can hold. A sub-second fraction and a non-zero offset are left alone: the
-- reader reads both correctly, and respelling either needs arithmetic this
-- file will not do.
--
-- `expires_at IS NULL` and `expires_at = ''` are the two spellings of "this
-- key does not expire" and are not touched by either arm.
UPDATE wafer_run__auth__api_keys
   SET revoked_at = COALESCE(NULLIF(revoked_at, ''), strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
       expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
       updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
 WHERE expires_at IS NOT NULL
   AND expires_at <> ''
   AND NOT (
         substr(expires_at, 1, 19) GLOB
         '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
         AND (
              substr(expires_at, 20) IN ('Z', 'z')
           OR substr(expires_at, 20) GLOB '[+-][0-9][0-9]:[0-9][0-9]'
           OR substr(expires_at, 20) GLOB '.[0-9]*'
         )
       );
