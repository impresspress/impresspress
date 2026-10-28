-- Give every share link minted under the old token scheme the expiry its
-- token used to carry, so the change of token scheme does not resurrect
-- links that are dead today.
--
-- A share token used to be a JWT signed with a fixed 30-day TTL, and the
-- public link handler verified that JWT (under `JwtExpPolicy::Required`)
-- before it ever read the share row. So a link stopped working 30 days
-- after it was minted, whatever the row said. A token is now opaque
-- entropy addressing one row, and the row's `expires_at` is the only thing
-- that can end a link.
--
-- Those two facts do not compose. The share modal used to post its expiry
-- under a field name the handler did not read, so essentially every
-- UI-created row carries no expiry at all -- and an unexpiring row plus an
-- opaque token is a permanently live public link. The links this repairs
-- are exactly the ones that are unreachable today, and before this release
-- they could not even be revoked: the revoke button sent the token to a
-- route keyed on the row id, so every revoke answered "not found".
--
-- A JWT-shaped token is the legacy one. The tokens this release mints are
-- 32 random bytes hex-encoded -- 64 characters, no dots -- so `LIKE
-- '%.%.%'` names the old scheme and nothing else, whenever this runs.
--
-- Rows that already carry an expiry are left alone: their owner chose it,
-- and it was already the binding one whenever it fell inside the JWT's 30
-- days.
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
UPDATE impresspress__files__cloud_shares
    SET expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', created_at, '+30 days'),
        updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE token LIKE '%.%.%'
      AND (expires_at IS NULL OR expires_at = '')
      AND strftime('%Y-%m-%dT%H:%M:%SZ', created_at, '+30 days') IS NOT NULL;

-- A legacy row whose `created_at` SQLite cannot read gets the instant this
-- migration runs. Its token is a JWT minted before this deployment was
-- upgraded, so the link is already dead by the rule above and an expiry of
-- "now" is what keeps it that way. Without this arm such a row would keep
-- a NULL expiry and come back to life.
UPDATE impresspress__files__cloud_shares
    SET expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
        updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE token LIKE '%.%.%'
      AND (expires_at IS NULL OR expires_at = '');
