-- Give every share link minted under the old token scheme the expiry its
-- token used to carry, so the change of token scheme neither resurrects a
-- link that is dead today nor kills one that still works.
--
-- A share token used to be a JWT, and the public link handler verified it
-- (under `JwtExpPolicy::Required`) before it ever read the share row. So a
-- link stopped working when its JWT aged out, whatever the row said. A
-- token is now opaque entropy addressing one row, and the row's
-- `expires_at` is the only thing that can end a link.
--
-- Those two facts do not compose. The share dialog used to post its expiry
-- under a field name the handler did not read, so essentially every
-- UI-created row carries no expiry at all -- and an unexpiring row plus an
-- opaque token is a permanently live public link. Before this release
-- those links could not even be revoked: the revoke button sent the token
-- to a route keyed on the row id, so every revoke answered "not found".
--
-- THE TTL CHANGED ONCE, so this repair has two arms. From the initial
-- commit until SEC-055 landed at 2026-05-14T05:43:03Z the JWT was signed
-- for 365 days; from then on, for 30. A row minted in the first period is
-- very likely STILL LIVE, and stamping it 30 days out would revoke a
-- working link -- the exact failure this migration exists to avoid, in the
-- other direction. So rows created before that instant get 365 days and
-- the rest get 30, each counted from `created_at`, which reproduces the
-- lifetime the token itself imposed. 365 days from `created_at` is also
-- within the one-year ceiling this release puts on new shares.
--
-- The cutoff is the instant the code changed, not the instant a given
-- deployment adopted it. A deployment that upgraded later has rows minted
-- after this instant that really ran the one-year code and will be given
-- 30 days here; that arm fails toward "a dead link stays dead", and the
-- remedy for a link that mattered is to share the file again.
--
-- A JWT-shaped token is the legacy one. The tokens this release mints are
-- 32 random bytes hex-encoded -- 64 characters, no dots -- so `LIKE
-- '%.%.%'` names the old scheme and nothing else, whenever this runs.
--
-- Rows that already carry an expiry are left alone: their owner chose it,
-- and it was already the binding one whenever it fell inside the token's
-- own life.
--
-- The stamp is written as RFC 3339 with a `Z` offset, which is what
-- `ShareRow::expires_at` is parsed as (`DateTime::parse_from_rfc3339`) and
-- what `handle_create_share` writes. SQLite's own `datetime()` format
-- (space-separated, no offset) would not parse, and the handler now
-- refuses a share whose expiry it cannot read -- fail-closed, but for the
-- wrong reason.
--
-- Re-running is harmless: after the first pass no legacy row still matches
-- the `expires_at IS NULL OR expires_at = ''` guard.

-- Arm 1: minted while the JWT was signed for a year.
UPDATE impresspress__files__cloud_shares
    SET expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', created_at, '+365 days'),
        updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE token LIKE '%.%.%'
      AND (expires_at IS NULL OR expires_at = '')
      AND created_at < '2026-05-14T05:43:03Z'
      AND strftime('%Y-%m-%dT%H:%M:%SZ', created_at, '+365 days') IS NOT NULL;

-- Arm 2: minted under the 30-day TTL.
UPDATE impresspress__files__cloud_shares
    SET expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', created_at, '+30 days'),
        updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE token LIKE '%.%.%'
      AND (expires_at IS NULL OR expires_at = '')
      AND strftime('%Y-%m-%dT%H:%M:%SZ', created_at, '+30 days') IS NOT NULL;

-- A legacy row whose `created_at` SQLite cannot read gets the instant this
-- migration runs. Neither arm above could place it, and the link is by now
-- past even the longer of the two lifetimes, so an expiry of "now" is what
-- keeps it dead. Without this arm such a row would keep a NULL expiry and
-- come back to life.
UPDATE impresspress__files__cloud_shares
    SET expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
        updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE token LIKE '%.%.%'
      AND (expires_at IS NULL OR expires_at = '');
