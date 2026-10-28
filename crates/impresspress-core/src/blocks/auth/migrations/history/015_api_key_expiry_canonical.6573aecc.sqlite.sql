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
-- WHAT IT DOES NOT CATCH. The arms below decide by SHAPE, and a shape is not
-- an instant: `2026-02-31T00:00:00Z` and a 29 February outside a leap year
-- are well-formed and name no day, so `repo::parse_iso` rejects them while
-- arm 2's test calls them readable. Such a row is left exactly as it is —
-- the reader refuses it, so the key is dead, but the admin API-keys tab
-- still shows it active and nothing says why. An operator who finds a key
-- that will not authenticate should revoke it there. Every other spelling
-- `parse_iso` rejects IS caught, because it is caught on shape.
--
-- Neither arm parses anything: every test below is a literal `substr`
-- comparison, so a stored string cannot make this migration fail, and a
-- migration that fails is never stamped and re-runs (and re-fails) on every
-- boot.
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
--
-- The `length(...)` pair is the same guard for the other way in. SQLite's
-- `substr` and `length` are character functions that STOP AT THE FIRST NUL,
-- so `'2026-06-01T12:00:00Z' || x'00' || 'junk'` passes every test above and
-- would be rewritten to the clean timestamp its prefix spells — resurrecting
-- a key `parse_iso` refuses. Casting to BLOB makes `length` count bytes
-- instead, and every spelling this arm accepts is pure ASCII, so the two
-- lengths agree for exactly the values that carry no NUL and no multi-byte
-- character.
UPDATE wafer_run__auth__api_keys
   SET expires_at = substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z',
       updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
 WHERE length(cast(expires_at AS BLOB)) = length(expires_at)
   AND substr(expires_at, 1, 19) GLOB
       '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
   AND substr(expires_at, 20) IN ('Z', 'z', '+00:00', '-00:00')
   AND expires_at <> substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z';

-- Arm 2: an expiry that names no instant. The key is already dead to the
-- reader; this records that as a revocation, so the admin API-keys tab shows
-- it revoked rather than active.
--
-- `expires_at` is left exactly as it was found. Overwriting it with the
-- repair's own timestamp would destroy the only record of why the key was
-- revoked, on a repair whose whole purpose is telling the operator that. The
-- column therefore keeps one format for every key that still works, and the
-- original text for the ones that do not.
--
-- A sub-second fraction and a non-zero offset are readable, so neither arm
-- touches them: the reader reads both correctly, and respelling either needs
-- arithmetic this file will not do.
--
-- `expires_at IS NULL` and `expires_at = ''` are the two spellings of "this
-- key does not expire" and are not touched by either arm. The `revoked_at`
-- guard makes a re-run a no-op rather than a re-stamp.
UPDATE wafer_run__auth__api_keys
   SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
       updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
 WHERE expires_at IS NOT NULL
   AND expires_at <> ''
   AND (revoked_at IS NULL OR revoked_at = '')
   AND NOT (
         length(cast(expires_at AS BLOB)) = length(expires_at)
         AND substr(expires_at, 1, 19) GLOB
             '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
         AND (
              substr(expires_at, 20) IN ('Z', 'z')
           OR substr(expires_at, 20) GLOB '[+-][0-9][0-9]:[0-9][0-9]'
           OR substr(expires_at, 20) GLOB '.[0-9]*'
         )
       );
