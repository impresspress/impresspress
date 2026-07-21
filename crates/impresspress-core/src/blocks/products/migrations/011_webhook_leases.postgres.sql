-- Preserve the exact signed event for integrity checks, support controlled
-- dead-letter replay, and record when an event became terminal.
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS payload_sha256 TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS payload_base64 TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS terminal_at TEXT;
