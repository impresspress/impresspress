-- Put `wafer_run__auth__api_keys.expires_at` into the one format the column
-- is meant to hold, and revoke the keys whose stored expiry is not a
-- timestamp at all. See `015_api_key_expiry_canonical.sqlite.sql` for the
-- full rationale; this file is the PostgreSQL dialect of the same change.
--
-- Neither arm casts to `timestamptz`. A cast is the one thing here that can
-- raise — `'2026-02-31T00:00:00Z'::timestamptz` is an error, and this column
-- held whatever an authenticated caller sent — and a migration that raises is
-- never stamped, so every later boot would re-run and re-fail the whole auth
-- set. Both arms are literal substring comparisons, exactly as the SQLite
-- dialect's `GLOB` tests are, so the two agree row for row.

-- Arm 1: a UTC expiry, respelled.
UPDATE wafer_run__auth__api_keys
   SET expires_at = substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z',
       updated_at = to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
 WHERE substr(expires_at, 1, 19) ~ '^\d{4}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}$'
   AND substr(expires_at, 20) IN ('Z', 'z', '+00:00', '-00:00')
   AND expires_at <> substr(expires_at, 1, 10) || 'T' || substr(expires_at, 12, 8) || 'Z';

-- Arm 2: an expiry that names no instant.
UPDATE wafer_run__auth__api_keys
   SET revoked_at = COALESCE(NULLIF(revoked_at, ''), to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')),
       expires_at = to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
       updated_at = to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
 WHERE expires_at IS NOT NULL
   AND expires_at <> ''
   AND NOT (
         substr(expires_at, 1, 19) ~ '^\d{4}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}$'
         AND (
              substr(expires_at, 20) IN ('Z', 'z')
           OR substr(expires_at, 20) ~ '^[+-]\d{2}:\d{2}$'
           OR substr(expires_at, 20) ~ '^\.\d'
         )
       );
