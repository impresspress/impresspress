-- Stripe webhook event idempotency (code review 2026-07-16: "Stripe
-- webhooks lack event idempotency").
--
-- `stripe::handle_webhook` records the top-level Stripe `event.id` here via
-- `INSERT ... ON CONFLICT (id) DO NOTHING` BEFORE running any side effect
-- (purchase completion, subscription sync, webhook fan-out). Stripe retries
-- undelivered/non-2xx webhooks, and the signature-timestamp window itself
-- accepts up to 5 minutes of replay — both redeliver the exact same `id`,
-- so a second delivery is a no-op once this row already exists.
--
-- `id` is the Stripe event id itself (e.g. `evt_1N...`), not a synthetic
-- key — the PRIMARY KEY is the UNIQUE constraint the insert-or-ignore
-- conflicts against.
--
-- Mirrored to 003_stripe_events.postgres.sql.
CREATE TABLE IF NOT EXISTS impresspress__products__stripe_events (
    id            TEXT PRIMARY KEY,
    event_type    TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL
);
