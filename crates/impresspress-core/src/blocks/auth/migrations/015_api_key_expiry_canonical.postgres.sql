-- Put `wafer_run__auth__api_keys.expires_at` into the one format the column
-- is meant to hold, and revoke the keys whose stored expiry names no instant.
-- See `015_api_key_expiry_canonical.sqlite.sql` for the full rationale,
-- including the grammar the "readable" test below accepts, which is exactly
-- what `repo::parse_iso` accepts; this file is the PostgreSQL dialect of the
-- same change. CI's PostgreSQL job runs the rows in
-- `015_api_key_expiry_canonical.cases.tsv` through this file, the same rows
-- the SQLite dialect is tested against.
--
-- Neither arm casts to `timestamptz` or to a number. A cast is the one thing
-- here that can raise — `'2026-02-31T00:00:00Z'::timestamptz` is an error,
-- and this column held whatever an authenticated caller sent — and a
-- migration that raises is never stamped, so every later boot would re-run
-- and re-fail the whole auth set. Every test is a regular expression or a
-- text comparison, so no stored string can make this file fail.
--
-- The readable test is one anchored regular expression for the shape and
-- the field ranges, plus three for the days a month does not have. Digits
-- are spelled `[0-9]`, never `\d`: `\d` is the locale's digit class, and
-- chrono reads ASCII digits only.
--
-- The SQLite dialect also refuses a value carrying an embedded NUL, which
-- would make its character-based `substr` read a truncated prefix.
-- PostgreSQL has no such case: `text` cannot hold a NUL byte at all.

-- Arm 1: a readable UTC expiry, respelled. The readable test is arm 2's,
-- repeated so an unreadable row — `2026-02-31T00:00:00+00:00`, say — reaches
-- arm 2 exactly as it was stored.
UPDATE wafer_run__auth__api_keys
   SET expires_at = substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z',
       updated_at = to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
 WHERE substr(expires_at, 20) IN ('Z', 'z', '+00:00', '-00:00', '−00:00')
   AND expires_at <> substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z'
   AND expires_at ~ '^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])[Tt ]([01][0-9]|2[0-3]):[0-5][0-9]:([0-5][0-9]|60)(\.[0-9]+)?([Zz]|[-+−]([01][0-9]|2[0-3]):[0-5][0-9])$'
   AND substr(expires_at, 6, 5) !~ '^(0[469]|11)-31$'
   AND substr(expires_at, 6, 5) !~ '^02-3'
   AND (substr(expires_at, 6, 5) <> '02-29'
        OR substr(expires_at, 1, 4) ~ '^([0-9]{2}(0[48]|[2468][048]|[13579][26])|(0[048]|[2468][048]|[13579][26])00)$');

-- Arm 2: an expiry that names no instant. `expires_at` is left as found —
-- it is the only record of why the key was revoked.
UPDATE wafer_run__auth__api_keys
   SET revoked_at = to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
       updated_at = to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
 WHERE expires_at IS NOT NULL
   AND expires_at <> ''
   AND (revoked_at IS NULL OR revoked_at = '')
   AND NOT (
         expires_at ~ '^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])[Tt ]([01][0-9]|2[0-3]):[0-5][0-9]:([0-5][0-9]|60)(\.[0-9]+)?([Zz]|[-+−]([01][0-9]|2[0-3]):[0-5][0-9])$'
     AND substr(expires_at, 6, 5) !~ '^(0[469]|11)-31$'
     AND substr(expires_at, 6, 5) !~ '^02-3'
     AND (substr(expires_at, 6, 5) <> '02-29'
          OR substr(expires_at, 1, 4) ~ '^([0-9]{2}(0[48]|[2468][048]|[13579][26])|(0[048]|[2468][048]|[13579][26])00)$')
       );
