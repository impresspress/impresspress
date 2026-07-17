-- Stripe webhook event idempotency. See 003_stripe_events.sqlite.sql for
-- the full rationale (including the pending/processed status semantics).
-- Same TEXT-only shape as the SQLite version — no backend-specific types
-- needed for this table.
CREATE TABLE IF NOT EXISTS impresspress__products__stripe_events (
    id            TEXT PRIMARY KEY,
    event_type    TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT 'pending',
    created_at    TEXT NOT NULL,
    updated_at    TEXT
);
