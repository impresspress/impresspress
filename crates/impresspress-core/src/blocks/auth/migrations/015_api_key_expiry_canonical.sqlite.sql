-- Put `wafer_run__auth__api_keys.expires_at` into the one format the column
-- is meant to hold, and revoke the keys whose stored expiry names no instant.
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
-- THE LINE BETWEEN THE ARMS IS `repo::parse_iso`, which is chrono's RFC 3339
-- parser, and the test below that stands for it accepts exactly what it
-- accepts:
--
--   YYYY-MM-DD, a real calendar day (0000-9999; 29 February only in a leap
--     year, which is a multiple of 4 that is not a century, or a multiple
--     of 400)
--   `T`, `t` or a space
--   hh:mm:ss with hh 00-23, mm 00-59, ss 00-60 (60 is a leap second)
--   optionally `.` and one or more digits, any number of them
--   `Z`, `z`, or a sign and hh:mm with hh 00-23 and mm 00-59. The sign is
--     `+`, `-`, or U+2212 MINUS SIGN, which chrono also takes.
--   and nothing after it.
--
-- `migrations::api_key_expiry_tests` holds that line to `parse_iso` itself:
-- every case in `015_api_key_expiry_canonical.cases.tsv` is asserted against
-- the parser AND against what this file does to the row, and CI's PostgreSQL
-- job runs the same cases through the PostgreSQL dialect.
--
-- Nothing below parses: every test is a `substr`, `GLOB`, `ltrim` or text
-- comparison, so a stored string cannot make this migration fail, and a
-- migration that fails is never stamped and re-runs (and re-fails) on every
-- boot. Every range test compares two ASCII digit strings of equal length
-- under SQLite's BINARY collation, which orders them as the numbers they
-- spell.
--
-- SQLite's `substr` and `length` are character functions that stop at the
-- first NUL, so `'2026-06-01T12:00:00Z' || x'00' || 'junk'` would pass every
-- test on its readable prefix. The `instr` over the BLOB cast compares bytes,
-- all of them, and refuses a value carrying a NUL anywhere.
--
-- Adding this file changes the auth block's SQL hash, so the upgrade that
-- applies it re-runs every auth migration, 012's sessions-table drop
-- included. RELEASE.md spells out what that costs.


-- Arm 1: a readable UTC expiry, respelled. `Z`, `z`, `+00:00`, `-00:00` and
-- `−00:00` all name the same offset, and characters 1-10 and 12-19 are the
-- date and the time whichever of them was used, so this rewrite cannot move
-- the instant. The exact `IN` list is deliberate: a wildcard would also match
-- a string that merely ENDS in `Z`, and rewriting `…T12:00:00<junk>Z` into a
-- valid timestamp would bring a key the reader refuses back to life.
--
-- The `IN` list fixes everything after character 19, so the rest of the
-- readable test is the date and the time, repeated from arm 2 verbatim. It
-- is repeated so an unreadable row — `2026-02-31T00:00:00+00:00`, say —
-- reaches arm 2 exactly as it was stored.
UPDATE wafer_run__auth__api_keys
   SET expires_at = substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z',
       updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
 WHERE substr(expires_at, 20) IN ('Z', 'z', '+00:00', '-00:00', '−00:00')
   AND expires_at <> substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z'
   AND instr(CAST(expires_at AS BLOB), x'00') = 0
   AND substr(expires_at, 1, 19) GLOB
       '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
   AND substr(expires_at, 6, 2) BETWEEN '01' AND '12'
   AND substr(expires_at, 9, 2) BETWEEN '01' AND
       CASE
         WHEN substr(expires_at, 6, 2) IN ('04', '06', '09', '11') THEN '30'
         WHEN substr(expires_at, 6, 2) <> '02' THEN '31'
         WHEN (substr(expires_at, 3, 2) GLOB '[02468][048]'
               OR substr(expires_at, 3, 2) GLOB '[13579][26]')
          AND (substr(expires_at, 3, 2) <> '00'
               OR substr(expires_at, 1, 2) GLOB '[02468][048]'
               OR substr(expires_at, 1, 2) GLOB '[13579][26]') THEN '29'
         ELSE '28'
       END
   AND substr(expires_at, 12, 2) <= '23'
   AND substr(expires_at, 15, 2) <= '59'
   AND substr(expires_at, 18, 2) <= '60';

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
-- The offset is found after the fraction, if there is one: `ltrim` strips the
-- fraction's digits, and the `.[0-9]` test before it is what makes "one or
-- more" of them compulsory. The inner `SELECT` names that offset once instead
-- of spelling its `CASE` out three times.
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
         instr(CAST(expires_at AS BLOB), x'00') = 0
     AND substr(expires_at, 1, 19) GLOB
         '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
     AND substr(expires_at, 6, 2) BETWEEN '01' AND '12'
     AND substr(expires_at, 9, 2) BETWEEN '01' AND
         CASE
           WHEN substr(expires_at, 6, 2) IN ('04', '06', '09', '11') THEN '30'
           WHEN substr(expires_at, 6, 2) <> '02' THEN '31'
           WHEN (substr(expires_at, 3, 2) GLOB '[02468][048]'
                 OR substr(expires_at, 3, 2) GLOB '[13579][26]')
            AND (substr(expires_at, 3, 2) <> '00'
                 OR substr(expires_at, 1, 2) GLOB '[02468][048]'
                 OR substr(expires_at, 1, 2) GLOB '[13579][26]') THEN '29'
           ELSE '28'
         END
     AND substr(expires_at, 12, 2) <= '23'
     AND substr(expires_at, 15, 2) <= '59'
     AND substr(expires_at, 18, 2) <= '60'
     AND (substr(expires_at, 20, 1) <> '.' OR substr(expires_at, 21, 1) GLOB '[0-9]')
     AND EXISTS (
           SELECT 1
             FROM (SELECT CASE WHEN substr(expires_at, 20, 1) = '.'
                               THEN ltrim(substr(expires_at, 21), '0123456789')
                               ELSE substr(expires_at, 20)
                          END AS tz) AS offset_part
            WHERE offset_part.tz IN ('Z', 'z')
               OR (substr(offset_part.tz, 1, 1) IN ('+', '-', '−')
                   AND substr(offset_part.tz, 2) GLOB '[0-9][0-9]:[0-5][0-9]'
                   AND substr(offset_part.tz, 2, 2) <= '23')
         )
       );
