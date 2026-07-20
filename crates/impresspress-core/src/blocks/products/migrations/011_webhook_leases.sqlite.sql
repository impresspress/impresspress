-- Preserve the exact signed event for integrity checks, support controlled
-- dead-letter replay, and record when an event became terminal.
ALTER TABLE impresspress__products__stripe_events ADD COLUMN payload_sha256 TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__stripe_events ADD COLUMN payload_base64 TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__stripe_events ADD COLUMN terminal_at TEXT;
